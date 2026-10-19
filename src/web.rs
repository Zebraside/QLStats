//! HTTP: axum-роутер, снапшоты (готовый JSON + gzip + ETag в ArcSwap), статика, таймер полуночи.

use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use arc_swap::ArcSwap;
use axum::Router;
use axum::body::Body;
use axum::error_handling::HandleErrorLayer;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bytes::Bytes;
use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use chrono_tz::Tz;
use rusqlite::Connection;
use rust_embed::RustEmbed;
use serde_json::{Value, json};
use tokio::sync::watch;
use tower::limit::GlobalConcurrencyLimitLayer;
use tower::load_shed::LoadShedLayer;
use tower::load_shed::error::Overloaded;
use tower::{BoxError, ServiceBuilder};
use tower_governor::GovernorError;
use tower_governor::GovernorLayer;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::{KeyExtractor, PeerIpKeyExtractor, SmartIpKeyExtractor};
use tower_http::timeout::TimeoutLayer;

use crate::collector::CollectorStats;
use crate::config::HttpLimits;
use crate::live::LiveHub;
use crate::{stats, store};

// ---------------------------------------------------------------- clock

/// Инжектируемые часы (в тестах — фиксированное время).
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> DateTime<Utc>;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

pub struct FixedClock(pub Mutex<DateTime<Utc>>);

impl FixedClock {
    pub fn new(t: DateTime<Utc>) -> Self {
        Self(Mutex::new(t))
    }
    pub fn set(&self, t: DateTime<Utc>) {
        *self.0.lock().unwrap() = t;
    }
}

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

pub fn fmt_time(t: DateTime<Utc>, tz: Tz) -> String {
    t.with_timezone(&tz)
        .to_rfc3339_opts(SecondsFormat::Secs, false)
}

pub fn today(now: DateTime<Utc>, tz: Tz) -> String {
    stats::day_key(now.timestamp_millis(), tz)
}

// ---------------------------------------------------------------- JSON builders

/// Тело `/api/summary`.
pub fn summary_json(conn: &Connection, tz: Tz, now: DateTime<Utc>) -> Result<Value> {
    let day = today(now, tz);
    let d = store::period_summary(conn, Some(&day))?;
    let a = store::period_summary(conn, None)?;
    Ok(json!({
        "generated_at": fmt_time(now, tz),
        "day": {
            "date": day,
            "matches": d.matches,
            "top_wins": d.top_wins,
            "top_kills": d.top_kills,
        },
        "all_time": a,
    }))
}

/// Тело `/api/players`.
pub fn players_json(conn: &Connection, tz: Tz, now: DateTime<Utc>) -> Result<Value> {
    Ok(json!({
        "generated_at": fmt_time(now, tz),
        "players": store::player_totals(conn)?,
    }))
}

// ---------------------------------------------------------------- cache

/// Готовый ответ: JSON, его gzip и ETag.
#[derive(Debug, Clone)]
pub struct Cached {
    pub body: Bytes,
    pub gzip: Bytes,
    pub etag: HeaderValue,
}

impl Cached {
    pub fn new(body: Vec<u8>, etag: &str) -> Self {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let _ = enc.write_all(&body);
        let gzip = enc.finish().unwrap_or_default();
        Self {
            body: Bytes::from(body),
            gzip: Bytes::from(gzip),
            etag: HeaderValue::from_str(etag).unwrap_or(HeaderValue::from_static("W/\"0\"")),
        }
    }
}

pub struct StatsSnapshot {
    pub day_key: String,
    pub summary: Cached,
    pub players: Cached,
}

pub struct AppState {
    pub db: Mutex<Connection>,
    pub tz: Tz,
    pub clock: Arc<dyn Clock>,
    pub live: Arc<LiveHub>,
    pub collector: Arc<CollectorStats>,
    pub static_dir: Option<PathBuf>,
    live_cache: ArcSwap<Cached>,
    live_key: Mutex<(u64, bool, i64)>,
    stats_cache: ArcSwap<StatsSnapshot>,
    boot: String,
    seq: AtomicU64,
    rebuild_lock: tokio::sync::Mutex<()>,
}

impl AppState {
    /// Создаёт состояние и сразу строит оба снапшота.
    pub fn new(
        db: Connection,
        tz: Tz,
        clock: Arc<dyn Clock>,
        live: Arc<LiveHub>,
        collector: Arc<CollectorStats>,
        static_dir: Option<PathBuf>,
    ) -> Result<Arc<Self>> {
        let now = clock.now();
        let boot = format!("{:x}", Utc::now().timestamp_millis());
        let empty = Cached::new(b"{}".to_vec(), "W/\"init\"");
        let st = Arc::new(Self {
            db: Mutex::new(db),
            tz,
            clock,
            live,
            collector,
            static_dir,
            live_cache: ArcSwap::from_pointee(empty.clone()),
            live_key: Mutex::new((0, false, 0)),
            stats_cache: ArcSwap::from_pointee(StatsSnapshot {
                day_key: String::new(),
                summary: empty.clone(),
                players: empty,
            }),
            boot,
            seq: AtomicU64::new(0),
            rebuild_lock: tokio::sync::Mutex::new(()),
        });
        st.rebuild_stats()?;
        st.refresh_live(now, true);
        Ok(st)
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Пересборка снапшота статистики (блокирующая: SQL-запросы).
    pub fn rebuild_stats(&self) -> Result<()> {
        let now = self.clock.now();
        let (summary, players) = {
            let conn = self.db();
            (
                summary_json(&conn, self.tz, now)?,
                players_json(&conn, self.tz, now)?,
            )
        };
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let etag = |k: &str| format!("W/\"{}-{k}{seq}\"", self.boot);
        self.stats_cache.store(Arc::new(StatsSnapshot {
            day_key: today(now, self.tz),
            summary: Cached::new(serde_json::to_vec(&summary)?, &etag("s")),
            players: Cached::new(serde_json::to_vec(&players)?, &etag("p")),
        }));
        tracing::info!(seq, "снапшот статистики пересобран");
        Ok(())
    }

    /// Проверяет устаревание live и пересобирает его JSON, если изменились версия или offline.
    pub fn refresh_live(&self, now: DateTime<Utc>, force: bool) {
        self.live.tick(now);
        let key = self.live.cache_key(now);
        let mut last = self.live_key.lock().unwrap_or_else(|e| e.into_inner());
        if !force && *last == key {
            return;
        }
        *last = key;
        let view = self.live.view(now);
        let body = serde_json::to_vec(&view).unwrap_or_default();
        let etag = format!("W/\"{}-l{}-{}-{:x}\"", self.boot, key.0, key.1 as u8, key.2);
        self.live_cache.store(Arc::new(Cached::new(body, &etag)));
    }

    pub fn live_cached(&self) -> Arc<Cached> {
        self.live_cache.load_full()
    }

    pub fn stats_snapshot(&self) -> Arc<StatsSnapshot> {
        self.stats_cache.load_full()
    }

    /// Страховка от сдвига часов: если день сменился, а таймер не сработал.
    async fn ensure_today(self: &Arc<Self>) {
        let today = today(self.clock.now(), self.tz);
        if self.stats_cache.load().day_key == today {
            return;
        }
        let _g = self.rebuild_lock.lock().await;
        if self.stats_cache.load().day_key == today {
            return;
        }
        let st = self.clone();
        match tokio::task::spawn_blocking(move || st.rebuild_stats()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::error!(error = %format!("{e:#}"), "ошибка пересборки снапшота"),
            Err(e) => tracing::error!(error = %e, "ошибка пересборки снапшота"),
        }
    }
}

// ---------------------------------------------------------------- handlers

fn etag_matches(headers: &HeaderMap, etag: &HeaderValue) -> bool {
    let Some(inm) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let strip = |s: &str| s.trim().trim_start_matches("W/").to_string();
    let mine = strip(etag.to_str().unwrap_or(""));
    inm.split(',').any(|t| t.trim() == "*" || strip(t) == mine)
}

fn accepts_gzip(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| {
            let mut gzip = None;
            let mut star = None;
            for p in s.split(',') {
                let mut it = p.split(';');
                let name = it.next().unwrap_or("").trim();
                let q = it
                    .filter_map(|x| x.trim().strip_prefix("q="))
                    .find_map(|v| v.trim().parse::<f32>().ok())
                    .unwrap_or(1.0);
                if name.eq_ignore_ascii_case("gzip") {
                    gzip = Some(q > 0.0);
                } else if name == "*" {
                    star = Some(q > 0.0);
                }
            }
            gzip.or(star).unwrap_or(false)
        })
}

fn not_modified(etag: &HeaderValue) -> Response {
    Response::builder()
        .status(StatusCode::NOT_MODIFIED)
        .header(header::ETAG, etag.clone())
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::empty())
        .unwrap()
}

fn serve_cached(headers: &HeaderMap, c: &Cached) -> Response {
    if etag_matches(headers, &c.etag) {
        return not_modified(&c.etag);
    }
    let mut b = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .header(header::ETAG, c.etag.clone())
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::VARY, "Accept-Encoding");
    let body = if accepts_gzip(headers) {
        b = b.header(header::CONTENT_ENCODING, "gzip");
        c.gzip.clone()
    } else {
        c.body.clone()
    };
    b.body(Body::from(body)).unwrap()
}

async fn api_live(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let c = st.live_cached();
    serve_cached(&headers, &c)
}

async fn api_summary(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    st.ensure_today().await;
    let s = st.stats_snapshot();
    serve_cached(&headers, &s.summary)
}

async fn api_players(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    st.ensure_today().await;
    let s = st.stats_snapshot();
    serve_cached(&headers, &s.players)
}

async fn healthz(State(st): State<Arc<AppState>>) -> Response {
    let st2 = st.clone();
    let db_ok = tokio::task::spawn_blocking(move || store::ping(&st2.db()).is_ok())
        .await
        .unwrap_or(false);
    let c = &st.collector;
    let body = json!({
        "status": if db_ok { "ok" } else { "error" },
        "version": env!("CARGO_PKG_VERSION"),
        "db": if db_ok { "ok" } else { "error" },
        "zmq": {
            "connected": c.connected.load(Ordering::Relaxed),
            "last_event_at": c.last_event_at().map(|t| fmt_time(t, st.tz)),
            "reconnects": c.reconnects.load(Ordering::Relaxed),
        }
    });
    let code = if db_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        code,
        [(header::CACHE_CONTROL, "no-cache")],
        axum::Json(body),
    )
        .into_response()
}

// ---------------------------------------------------------------- static

#[derive(RustEmbed)]
#[folder = "static/"]
struct Assets;

fn safe_rel(path: &str) -> Option<PathBuf> {
    let p = Path::new(path);
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => {
                if s.to_string_lossy().starts_with('.') {
                    return None;
                }
                out.push(s)
            }
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

async fn static_file(State(st): State<Arc<AppState>>, uri: Uri, headers: HeaderMap) -> Response {
    let mut path = uri.path().trim_start_matches('/').to_string();
    if path.is_empty() || path.ends_with('/') {
        path.push_str("index.html");
    }
    let Some(rel) = safe_rel(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = mime_guess::from_path(&rel).first_or_octet_stream();
    let (data, etag): (Bytes, String) = if let Some(dir) = &st.static_dir {
        match tokio::fs::read(dir.join(&rel)).await {
            Ok(d) => {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                std::hash::Hash::hash(&d, &mut h);
                let tag = format!("\"d{:x}\"", std::hash::Hasher::finish(&h));
                (Bytes::from(d), tag)
            }
            Err(_) => return StatusCode::NOT_FOUND.into_response(),
        }
    } else {
        match Assets::get(&rel.to_string_lossy().replace('\\', "/")) {
            Some(f) => {
                let hash = f.metadata.sha256_hash();
                let tag: String = hash[..8].iter().map(|b| format!("{b:02x}")).collect();
                (Bytes::from(f.data.into_owned()), format!("\"e{tag}\""))
            }
            None => return StatusCode::NOT_FOUND.into_response(),
        }
    };
    let etag = HeaderValue::from_str(&etag).unwrap_or(HeaderValue::from_static("\"0\""));
    if etag_matches(&headers, &etag) {
        return not_modified(&etag);
    }
    let ct = if mime.type_() == "text" || mime.subtype() == "javascript" {
        format!("{mime}; charset=utf-8")
    } else {
        mime.to_string()
    };
    Response::builder()
        .header(header::CONTENT_TYPE, ct)
        .header(header::ETAG, etag)
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(data))
        .unwrap()
}

// ---------------------------------------------------------------- router & tasks

/// Как часто чистить хранилище per-IP квот от давно не приходивших клиентов.
const RATE_LIMIT_CLEANUP: Duration = Duration::from_secs(60);

/// Роутер со всеми слоями. Снаружи внутрь: trace → timeout → load shed +
/// concurrency limit → per-IP rate limit → compression → обработчики.
///
/// `Router::layer` оборачивает каждый маршрут отдельно, поэтому предел
/// одновременных запросов — `GlobalConcurrencyLimitLayer` (общий семафор на все
/// маршруты), а квоты governor общие через `Arc` в его конфиге.
///
/// Без `trust_proxy` ключ квоты — peer IP из `ConnectInfo<SocketAddr>`: сервер
/// надо запускать через `into_make_service_with_connect_info` (см. [`serve`]).
pub fn router(state: Arc<AppState>, limits: &HttpLimits) -> Router {
    let mut app = Router::new()
        .route("/api/live", get(api_live))
        .route("/api/summary", get(api_summary))
        .route("/api/players", get(api_players))
        .route("/healthz", get(healthz))
        .fallback(get(static_file))
        .layer(tower_http::compression::CompressionLayer::new().gzip(true));
    if limits.rate_limit_per_sec > 0 {
        app = if limits.trust_proxy {
            with_rate_limit(app, SmartIpKeyExtractor, limits)
        } else {
            with_rate_limit(app, PeerIpKeyExtractor, limits)
        };
    }
    if limits.max_concurrency > 0 {
        // Переполнение — сразу 503, без очереди.
        app = app.layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(handle_overload))
                .layer(LoadShedLayer::new())
                .layer(GlobalConcurrencyLimitLayer::new(limits.max_concurrency)),
        );
    }
    if limits.request_timeout_secs > 0 {
        app = app.layer(TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            Duration::from_secs(limits.request_timeout_secs),
        ));
    }
    app.layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

/// Per-IP квота (GCRA): `rate_limit_burst` запросов подряд, восстановление
/// `rate_limit_per_sec` в секунду. Превышение — 429 с `Retry-After`.
fn with_rate_limit<K>(
    app: Router<Arc<AppState>>,
    key: K,
    limits: &HttpLimits,
) -> Router<Arc<AppState>>
where
    K: KeyExtractor<Key = IpAddr> + Send + Sync + 'static,
{
    // В tower_governor `per_second(n)` — это период «один запрос раз в n секунд»,
    // поэтому период задаём сами.
    let period = Duration::from_nanos((1_000_000_000 / limits.rate_limit_per_sec).max(1));
    let config = GovernorConfigBuilder::default()
        .key_extractor(key)
        .period(period)
        .burst_size(limits.rate_limit_burst.max(1))
        .finish()
        .expect("период и ёмкость квоты не нулевые");
    // Раз в минуту выбрасываем из хранилища квот IP, которые давно не приходили,
    // чтобы память не росла. Задача завершается вместе с роутером (по `Weak`).
    let limiter = Arc::downgrade(config.limiter());
    spawn_periodic(move || match limiter.upgrade() {
        Some(l) => {
            l.retain_recent();
            l.shrink_to_fit();
            true
        }
        None => false,
    });
    app.layer(GovernorLayer::new(config).error_handler(rate_limit_response))
}

/// Раз в [`RATE_LIMIT_CLEANUP`] вызывает `step`, пока тот возвращает `true`.
/// Вне tokio-рантайма ничего не делает.
fn spawn_periodic(mut step: impl FnMut() -> bool + Send + 'static) {
    let Ok(rt) = tokio::runtime::Handle::try_current() else {
        return;
    };
    rt.spawn(async move {
        let mut tick = tokio::time::interval(RATE_LIMIT_CLEANUP);
        tick.tick().await;
        loop {
            tick.tick().await;
            if !step() {
                break;
            }
        }
    });
}

fn rate_limit_response(e: GovernorError) -> Response {
    match e {
        GovernorError::TooManyRequests { wait_time, .. } => (
            StatusCode::TOO_MANY_REQUESTS,
            [
                // governor округляет вниз; `Retry-After: 0` провоцирует немедленный повтор.
                (header::RETRY_AFTER, wait_time.max(1).to_string()),
                (header::CACHE_CONTROL, "no-store".to_string()),
            ],
            "слишком много запросов\n",
        )
            .into_response(),
        other => Response::<Body>::from(other),
    }
}

async fn handle_overload(err: BoxError) -> Response {
    if err.is::<Overloaded>() {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            [
                (header::RETRY_AFTER, "1"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            "сервер перегружен\n",
        )
            .into_response()
    } else {
        tracing::error!(error = %err, "ошибка middleware");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

/// Время до следующей полуночи в `tz`.
pub fn until_next_midnight(now: DateTime<Utc>, tz: Tz) -> Duration {
    let local = now.with_timezone(&tz);
    let next = local
        .date_naive()
        .succ_opt()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .and_then(|dt| tz.from_local_datetime(&dt).earliest());
    match next {
        Some(n) => (n.with_timezone(&Utc) - now)
            .to_std()
            .unwrap_or(Duration::from_secs(1)),
        None => Duration::from_secs(3600),
    }
}

/// Фоновые задачи: пересборка статистики по сигналу writer'а и в полночь, обновление live.
pub fn spawn_background(state: Arc<AppState>, mut data_rx: watch::Receiver<u64>) {
    // Статистика по сигналу "data changed".
    let st = state.clone();
    tokio::spawn(async move {
        while data_rx.changed().await.is_ok() {
            let _g = st.rebuild_lock.lock().await;
            let s2 = st.clone();
            if let Ok(Err(e)) = tokio::task::spawn_blocking(move || s2.rebuild_stats()).await {
                tracing::error!(error = %format!("{e:#}"), "ошибка пересборки снапшота");
            }
        }
    });
    // Полночь по tz.
    let st = state.clone();
    tokio::spawn(async move {
        loop {
            let wait = until_next_midnight(st.clock.now(), st.tz) + Duration::from_secs(1);
            tracing::debug!(secs = wait.as_secs(), "до пересборки в полночь");
            tokio::time::sleep(wait).await;
            let _g = st.rebuild_lock.lock().await;
            let s2 = st.clone();
            if let Ok(Err(e)) = tokio::task::spawn_blocking(move || s2.rebuild_stats()).await {
                tracing::error!(error = %format!("{e:#}"), "ошибка пересборки снапшота в полночь");
            }
        }
    });
    // Live: по изменению версии (дебаунс 200 мс) и раз в секунду (offline, устаревание).
    let st = state;
    tokio::spawn(async move {
        let mut rx = st.live.subscribe();
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                r = rx.changed() => {
                    if r.is_err() { break; }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                _ = tick.tick() => {}
            }
            st.refresh_live(st.clock.now(), false);
        }
    });
}

/// Запускает HTTP-сервер до сигнала остановки.
pub async fn serve(
    state: Arc<AppState>,
    addr: SocketAddr,
    limits: &HttpLimits,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, ?limits, "HTTP-сервер запущен");
    // ConnectInfo нужен PeerIpKeyExtractor'у (и SmartIpKeyExtractor'у без заголовков).
    let app = router(state, limits).into_make_service_with_connect_info::<SocketAddr>();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn midnight_msk() {
        let tz: Tz = "Europe/Moscow".parse().unwrap();
        let now = DateTime::parse_from_rfc3339("2026-10-18T20:59:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(until_next_midnight(now, tz), Duration::from_secs(60));
    }

    #[test]
    fn etag_compare() {
        let mut h = HeaderMap::new();
        let e = HeaderValue::from_static("W/\"abc-s1\"");
        h.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_static("\"x\", W/\"abc-s1\""),
        );
        assert!(etag_matches(&h, &e));
        h.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_static("\"abc-s2\""),
        );
        assert!(!etag_matches(&h, &e));
    }

    #[test]
    fn gzip_negotiation() {
        let mut h = HeaderMap::new();
        h.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("gzip, deflate, br"),
        );
        assert!(accepts_gzip(&h));
        h.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("br;q=1, gzip;q=0"),
        );
        assert!(!accepts_gzip(&h));
        h.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("gzip;q=0.0, *"),
        );
        assert!(!accepts_gzip(&h));
        h.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("br, *;q=0.5"),
        );
        assert!(accepts_gzip(&h));
    }
}
