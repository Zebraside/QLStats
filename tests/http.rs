//! HTTP-тесты роутера: формат JSON, ETag/304, gzip, смена суток, ограничение нагрузки.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use chrono::Duration;
use common::*;
use http_body_util::BodyExt;
use qlstats::collector::CollectorStats;
use qlstats::config::HttpLimits;
use qlstats::live::LiveHub;
use qlstats::store;
use qlstats::web::{self, AppState, Clock, FixedClock};
use serde_json::Value;
use tower::ServiceExt;

struct Env {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
    clock: Arc<FixedClock>,
}

fn setup() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let (w, mut live, last) = replay_into(&db, "synthetic.jsonl");
    drop(w);
    live.set_link(true, last);
    let clock = Arc::new(FixedClock::new(last));
    let hub = Arc::new(LiveHub::new(live, 15, tz()));
    let state = AppState::new(
        store::open(&db).unwrap(),
        tz(),
        clock.clone(),
        hub,
        Arc::new(CollectorStats::default()),
        None,
    )
    .unwrap();
    Env {
        _dir: dir,
        state,
        clock,
    }
}

async fn get(
    env: &Env,
    uri: &str,
    inm: Option<&str>,
    gzip: bool,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut req = Request::builder().uri(uri);
    if let Some(e) = inm {
        req = req.header(header::IF_NONE_MATCH, e);
    }
    if gzip {
        req = req.header(header::ACCEPT_ENCODING, "gzip");
    }
    // Лимиты по умолчанию: ConnectInfo подставляем, как это делает `serve`.
    let resp = web::router(env.state.clone(), &HttpLimits::default())
        .oneshot(with_peer(req.body(Body::empty()).unwrap(), "10.0.0.1"))
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, headers, body)
}

/// Добавляет peer-адрес, который в реальном сервере кладёт
/// `into_make_service_with_connect_info`.
fn with_peer(mut req: Request<Body>, ip: &str) -> Request<Body> {
    let addr = SocketAddr::new(ip.parse().unwrap(), 40000);
    req.extensions_mut().insert(ConnectInfo(addr));
    req
}

fn req(uri: &str, peer: &str, xff: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().uri(uri);
    if let Some(x) = xff {
        b = b.header("x-forwarded-for", x);
    }
    with_peer(b.body(Body::empty()).unwrap(), peer)
}

/// Статус ответа и `Retry-After` (если есть) от уже собранного роутера.
async fn status(app: &Router, r: Request<Body>) -> (StatusCode, Option<u64>) {
    let resp = app.clone().oneshot(r).await.unwrap();
    let retry = resp
        .headers()
        .get(header::RETRY_AFTER)
        .map(|v| v.to_str().unwrap().parse().unwrap());
    (resp.status(), retry)
}

fn rate_limits(trust_proxy: bool) -> HttpLimits {
    HttpLimits {
        rate_limit_per_sec: 1,
        rate_limit_burst: 2,
        trust_proxy,
        ..HttpLimits::disabled()
    }
}

#[tokio::test]
async fn rate_limit_per_peer_ip() {
    let env = setup();
    let app = web::router(env.state.clone(), &rate_limits(false));
    for _ in 0..2 {
        let (st, _) = status(&app, req("/api/live", "10.0.0.1", None)).await;
        assert_eq!(st, StatusCode::OK);
    }
    let (st, retry) = status(&app, req("/api/live", "10.0.0.1", None)).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    assert!(retry.is_some_and(|s| s >= 1), "{retry:?}");
    // Другой IP — своя квота; /healthz под тем же лимитом.
    let (st, _) = status(&app, req("/healthz", "10.0.0.2", None)).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = status(&app, req("/healthz", "10.0.0.1", None)).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn rate_limit_trust_proxy_uses_forwarded_for() {
    let env = setup();
    let app = web::router(env.state.clone(), &rate_limits(true));
    // Все запросы приходят от прокси 127.0.0.1, клиенты различаются по XFF.
    for _ in 0..2 {
        let (st, _) = status(&app, req("/api/live", "127.0.0.1", Some("203.0.113.5"))).await;
        assert_eq!(st, StatusCode::OK);
    }
    let (st, retry) = status(&app, req("/api/live", "127.0.0.1", Some("203.0.113.5"))).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    assert!(retry.is_some_and(|s| s >= 1), "{retry:?}");
    let (st, _) = status(&app, req("/api/live", "127.0.0.1", Some("203.0.113.6"))).await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn rate_limit_ignores_forwarded_for_without_trust_proxy() {
    let env = setup();
    let app = web::router(env.state.clone(), &rate_limits(false));
    // Разные XFF с одного peer делят одну квоту.
    for xff in ["203.0.113.5", "203.0.113.6"] {
        let (st, _) = status(&app, req("/api/live", "10.0.0.1", Some(xff))).await;
        assert_eq!(st, StatusCode::OK);
    }
    let (st, _) = status(&app, req("/api/live", "10.0.0.1", Some("203.0.113.7"))).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn load_shed_when_concurrency_exhausted() {
    let env = setup();
    let limits = HttpLimits {
        max_concurrency: 1,
        ..HttpLimits::disabled()
    };
    let app = web::router(env.state.clone(), &limits);

    // /healthz ждёт мьютекс БД в spawn_blocking: держим его, и медленный запрос
    // занимает единственный слот, пока мы не отпустим.
    // Мьютекс держит отдельный поток (не держим std-блокировку через await);
    // при панике теста `release` уничтожается и поток отпускает мьютекс.
    let (release, release_rx) = std::sync::mpsc::channel::<()>();
    let (locked_tx, locked_rx) = std::sync::mpsc::channel::<()>();
    let st = env.state.clone();
    let holder = std::thread::spawn(move || {
        let _g = st.db.lock().unwrap();
        locked_tx.send(()).unwrap();
        let _ = release_rx.recv();
    });
    locked_rx.recv().unwrap();
    let spawn_slow = || tokio::spawn(app.clone().oneshot(req("/healthz", "10.0.0.1", None)));
    let mut slow = spawn_slow();

    // /api/live БД не трогает. Пока медленный запрос не занял слот, ответ 200;
    // как только занял — 503 сразу, без ожидания в очереди. Пауза перед пробой
    // даёт медленному запросу дойти до обработчика; если проба всё же успела
    // занять слот первой и медленный запрос сам получил 503, запускаем его заново.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let shed = loop {
        assert!(tokio::time::Instant::now() < deadline, "503 не дождались");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        if slow.is_finished() {
            let resp = slow.await.unwrap().unwrap();
            assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
            slow = spawn_slow();
            continue;
        }
        let (st, retry) = status(&app, req("/api/live", "10.0.0.2", None)).await;
        if st == StatusCode::SERVICE_UNAVAILABLE {
            break retry;
        }
        assert_eq!(st, StatusCode::OK);
    };
    assert_eq!(shed, Some(1));

    drop(release);
    holder.join().unwrap();
    let resp = slow.await.unwrap().unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let (st, _) = status(&app, req("/api/live", "10.0.0.2", None)).await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn summary_etag_304() {
    let env = setup();
    let (st, h, body) = get(&env, "/api/summary", None, false).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(h[header::CACHE_CONTROL], "no-cache");
    let etag = h[header::ETAG].to_str().unwrap().to_string();
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["day"]["date"], "2026-10-19");
    assert_eq!(v["day"]["matches"], 1);
    assert_eq!(v["all_time"]["matches"], 2);
    assert!(v["generated_at"].as_str().unwrap().ends_with("+03:00"));
    let (st, h, body) = get(&env, "/api/summary", Some(&etag), false).await;
    assert_eq!(st, StatusCode::NOT_MODIFIED);
    assert_eq!(h[header::ETAG].to_str().unwrap(), etag);
    assert!(body.is_empty());
    let (st, _, _) = get(&env, "/api/summary", Some("W/\"other\""), false).await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn players_format_and_gzip() {
    let env = setup();
    let (st, h, body) = get(&env, "/api/players", None, true).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(h[header::CONTENT_ENCODING], "gzip");
    let mut d = flate2::read::GzDecoder::new(&body[..]);
    let mut s = String::new();
    std::io::Read::read_to_string(&mut d, &mut s).unwrap();
    let v: Value = serde_json::from_str(&s).unwrap();
    let players = v["players"].as_array().unwrap();
    assert_eq!(players.len(), 3);
    let keys: Vec<&str> = players[0]
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    assert_eq!(
        keys,
        [
            "steam_id",
            "name",
            "matches",
            "kills",
            "deaths",
            "best_streak",
            "wins",
            "accuracy",
            "suicides",
            "env_deaths",
            "fav_weapon"
        ]
    );
    let etag = h[header::ETAG].to_str().unwrap().to_string();
    let (st, _, _) = get(&env, "/api/players", Some(&etag), true).await;
    assert_eq!(st, StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn live_etag_changes_with_state() {
    let env = setup();
    let (st, h, body) = get(&env, "/api/live", None, false).await;
    assert_eq!(st, StatusCode::OK);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["state"], "warmup");
    assert_eq!(v["score_approximate"], true);
    assert!(v["match"].is_null());
    let etag = h[header::ETAG].to_str().unwrap().to_string();
    let (st, _, _) = get(&env, "/api/live", Some(&etag), false).await;
    assert_eq!(st, StatusCode::NOT_MODIFIED);

    // Новое событие меняет версию и ETag.
    let ev = qlstats::model::parse(
        r#"{"TYPE":"MATCH_STARTED","DATA":{"MATCH_GUID":"g","GAME_TYPE":"FFA","MAP":"dm6","FACTORY_TITLE":"Free For All","FRAG_LIMIT":20,"TIME_LIMIT":10,"PLAYERS":[{"NAME":"Bobby","STEAM_ID":"76561190000000002","TEAM":0}]}}"#,
    )
    .unwrap();
    let now = env.clock.now();
    env.state.live.apply(&ev.event, now);
    env.state.refresh_live(now, false);
    let (st, h2, body) = get(&env, "/api/live", Some(&etag), false).await;
    assert_eq!(st, StatusCode::OK);
    assert_ne!(h2[header::ETAG].to_str().unwrap(), etag);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["state"], "in_progress");
    assert_eq!(v["match"]["map"], "dm6");
    assert_eq!(v["match"]["limits"]["frag"], 20);
    assert_eq!(v["match"]["limits"]["time_min"], 10);
    assert_eq!(v["players"][0]["score"], 0);
}

#[tokio::test]
async fn day_rollover_rebuilds_snapshot() {
    let env = setup();
    let (_, h, _) = get(&env, "/api/summary", None, false).await;
    let etag = h[header::ETAG].to_str().unwrap().to_string();
    env.clock.set(env.clock.now() + Duration::days(1));
    let (st, _, body) = get(&env, "/api/summary", Some(&etag), false).await;
    assert_eq!(st, StatusCode::OK);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["day"]["date"], "2026-10-20");
    assert_eq!(v["day"]["matches"], 0);
    assert_eq!(v["day"]["top_wins"]["value"], 0);
    assert_eq!(v["day"]["top_wins"]["players"].as_array().unwrap().len(), 0);
    assert_eq!(v["all_time"]["matches"], 2);
}

#[tokio::test]
async fn healthz_ok() {
    let env = setup();
    let (st, _, body) = get(&env, "/healthz", None, false).await;
    assert_eq!(st, StatusCode::OK);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["status"], "ok");
    assert_eq!(v["db"], "ok");
    assert_eq!(v["zmq"]["connected"], false);
}

#[tokio::test]
async fn static_and_404() {
    let env = setup();
    let (st, _, _) = get(&env, "/../Cargo.toml", None, false).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _, _) = get(&env, "/no-such-file.txt", None, false).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // index.html есть, если фронтенд уже положен в static/.
    if Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("static/index.html")
        .exists()
    {
        let (st, h, _) = get(&env, "/", None, false).await;
        assert_eq!(st, StatusCode::OK);
        assert!(
            h[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/html")
        );
        let etag = h[header::ETAG].to_str().unwrap().to_string();
        let (st, _, _) = get(&env, "/", Some(&etag), false).await;
        assert_eq!(st, StatusCode::NOT_MODIFIED);
    }
}

use std::path::Path;
