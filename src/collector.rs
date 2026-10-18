//! ZMQ SUB: подключение с PLAIN, socket monitor, переподключение, режим записи JSONL.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use serde_json::{Value, json};

use crate::config::ZmqConfig;
use crate::live::LiveHub;
use crate::model;
use crate::store::WriterMsg;

/// Счётчики для `/healthz`.
#[derive(Debug, Default)]
pub struct CollectorStats {
    pub connected: AtomicBool,
    /// UTC мс последнего полученного фрейма, 0 — не было.
    pub last_event_ms: AtomicI64,
    pub reconnects: AtomicU64,
}

impl CollectorStats {
    pub fn last_event_at(&self) -> Option<DateTime<Utc>> {
        match self.last_event_ms.load(Ordering::Relaxed) {
            0 => None,
            ms => Utc.timestamp_millis_opt(ms).single(),
        }
    }
}

/// Тишина, после которой пишется WARN.
const SILENCE_WARN: Duration = Duration::from_secs(10 * 60);
/// Тишина при «живом» соединении, после которой сокет пересоздаётся.
const SILENCE_RECREATE: Duration = Duration::from_secs(30 * 60);
/// Сколько ждать успешного handshake, прежде чем пересоздать сокет.
const NO_LINK_RECREATE: Duration = Duration::from_secs(60);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

fn monitor_event_name(e: u16) -> &'static str {
    match e {
        0x0001 => "CONNECTED",
        0x0002 => "CONNECT_DELAYED",
        0x0004 => "CONNECT_RETRIED",
        0x0008 => "LISTENING",
        0x0010 => "BIND_FAILED",
        0x0020 => "ACCEPTED",
        0x0040 => "ACCEPT_FAILED",
        0x0080 => "CLOSED",
        0x0100 => "CLOSE_FAILED",
        0x0200 => "DISCONNECTED",
        0x0400 => "MONITOR_STOPPED",
        0x0800 => "HANDSHAKE_FAILED_NO_DETAIL",
        0x1000 => "HANDSHAKE_SUCCEEDED",
        0x2000 => "HANDSHAKE_FAILED_PROTOCOL",
        0x4000 => "HANDSHAKE_FAILED_AUTH",
        _ => "UNKNOWN",
    }
}

/// Событие монитора: (код, значение, адрес).
fn read_monitor(mon: &zmq::Socket) -> Vec<(u16, u32, String)> {
    let mut out = Vec::new();
    while let Ok(parts) = mon.recv_multipart(zmq::DONTWAIT) {
        if let Some(f) = parts.first()
            && f.len() >= 6
        {
            let ev = u16::from_le_bytes([f[0], f[1]]);
            let val = u32::from_le_bytes([f[2], f[3], f[4], f[5]]);
            let addr = parts
                .get(1)
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .unwrap_or_default();
            out.push((ev, val, addr));
        }
    }
    out
}

/// Создаёт SUB-сокет с PLAIN и монитором и подключается.
fn open_sub(ctx: &zmq::Context, cfg: &ZmqConfig, tag: &str) -> Result<(zmq::Socket, zmq::Socket)> {
    let sock = ctx.socket(zmq::SUB)?;
    sock.set_plain_username(Some(&cfg.user))?;
    sock.set_plain_password(Some(cfg.password.expose()))?;
    sock.set_zap_domain("stats")?;
    sock.set_reconnect_ivl(1000)?;
    sock.set_reconnect_ivl_max(30_000)?;
    sock.set_heartbeat_ivl(10_000)?;
    sock.set_heartbeat_timeout(30_000)?;
    sock.set_heartbeat_ttl(60_000)?;
    sock.set_rcvtimeo(1000)?;
    sock.set_linger(0)?;
    sock.set_subscribe(b"")?;
    let mon_ep = format!("inproc://qlstats-monitor-{tag}");
    sock.monitor(&mon_ep, zmq::SocketEvent::ALL as i32)?;
    let mon = ctx.socket(zmq::PAIR)?;
    mon.connect(&mon_ep)?;
    let ep = cfg.endpoint();
    tracing::info!(endpoint = %ep, user = %cfg.user, "подключение к ZMQ");
    sock.connect(&ep).with_context(|| format!("connect {ep}"))?;
    Ok((sock, mon))
}

/// Состояние одного коллектора.
struct Collector {
    cfg: ZmqConfig,
    hub: Arc<LiveHub>,
    writer: mpsc::Sender<WriterMsg>,
    stats: Arc<CollectorStats>,
    stop: Arc<AtomicBool>,
    connected: bool,
    ever_connected: bool,
    session_had_link: bool,
}

impl Collector {
    fn link(&mut self, up: bool) {
        if up == self.connected {
            return;
        }
        self.connected = up;
        self.stats.connected.store(up, Ordering::Relaxed);
        let now = Utc::now();
        self.hub.set_link(up, now);
        let _ = self.writer.send(WriterMsg::Link {
            connected: up,
            at: now.timestamp_millis(),
        });
        if up {
            if self.ever_connected {
                self.stats.reconnects.fetch_add(1, Ordering::Relaxed);
            }
            self.ever_connected = true;
        }
    }

    fn on_monitor(&mut self, ev: u16, val: u32, addr: &str) {
        let name = monitor_event_name(ev);
        match ev {
            0x1000 => {
                tracing::info!(%addr, "ZMQ: handshake PLAIN успешен, соединение установлено");
                self.link(true);
            }
            0x0001 => tracing::info!(%addr, "ZMQ: TCP-соединение установлено"),
            0x0200 => {
                tracing::warn!(%addr, "ZMQ: соединение разорвано");
                self.link(false);
            }
            0x4000 => {
                tracing::error!(%addr, status = val, "ZMQ: ошибка аутентификации PLAIN");
                self.link(false);
            }
            0x0800 | 0x2000 => {
                tracing::warn!(%addr, event = name, value = val, "ZMQ: ошибка handshake");
                self.link(false);
            }
            0x0004 => {
                tracing::debug!(%addr, "ZMQ: повтор подключения");
                self.link(false);
            }
            0x0080 => {
                tracing::debug!(%addr, "ZMQ: сокет закрыт");
                self.link(false);
            }
            _ => tracing::debug!(%addr, event = name, value = val, "ZMQ monitor"),
        }
    }

    fn on_frame(&mut self, bytes: Vec<u8>) {
        let now = Utc::now();
        let ms = now.timestamp_millis();
        self.stats.last_event_ms.store(ms, Ordering::Relaxed);
        if !self.connected {
            // Данные пришли — значит, соединение живо, даже если монитор опоздал.
            self.link(true);
        }
        let text = model::decode_lossy(&bytes);
        match model::parse(&text) {
            Ok(p) => {
                tracing::debug!(kind = %p.kind, guid = p.event.match_guid().unwrap_or(""), "событие");
                self.hub.apply(&p.event, now);
            }
            Err(e) => tracing::warn!(error = %e, "не удалось разобрать событие"),
        }
        if self
            .writer
            .send(WriterMsg::Frame {
                received_at: ms,
                bytes,
            })
            .is_err()
        {
            tracing::error!("writer недоступен, событие потеряно");
        }
    }

    /// Один «сеанс» сокета; признак связи в сеансе — `session_had_link`.
    fn session(&mut self, n: u64) -> Result<bool> {
        let ctx = zmq::Context::new();
        let (sock, mon) = open_sub(&ctx, &self.cfg, &n.to_string())?;
        let mut last_data = Instant::now();
        let mut down_since = Some(Instant::now());
        let mut warned_silence = false;
        self.session_had_link = false;
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return Ok(true);
            }
            for (ev, val, addr) in read_monitor(&mon) {
                self.on_monitor(ev, val, &addr);
            }
            if self.connected {
                self.session_had_link = true;
                down_since = None;
            } else if down_since.is_none() {
                down_since = Some(Instant::now());
            }
            match sock.recv_bytes(0) {
                Ok(bytes) => {
                    last_data = Instant::now();
                    warned_silence = false;
                    self.on_frame(bytes);
                    continue;
                }
                Err(zmq::Error::EAGAIN) | Err(zmq::Error::EINTR) => {}
                Err(e) => return Err(anyhow!("ошибка recv: {e}")),
            }
            if let Some(ds) = down_since
                && ds.elapsed() > NO_LINK_RECREATE
            {
                return Err(anyhow!(
                    "нет соединения {} с, сокет будет пересоздан",
                    ds.elapsed().as_secs()
                ));
            }
            let silent = last_data.elapsed();
            if self.connected && silent > SILENCE_WARN && !warned_silence {
                tracing::warn!(secs = silent.as_secs(), "ZMQ: долго нет событий");
                warned_silence = true;
            }
            if self.connected && silent > SILENCE_RECREATE {
                tracing::info!(
                    secs = silent.as_secs(),
                    "ZMQ: долгая тишина, сокет пересоздаётся"
                );
                self.link(false);
                return Ok(true);
            }
        }
    }

    fn run(mut self) {
        let mut backoff = Duration::from_secs(1);
        let mut n = 0u64;
        while !self.stop.load(Ordering::Relaxed) {
            n += 1;
            match self.session(n) {
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, pause_secs = backoff.as_secs(), "ZMQ: пересоздание сокета");
                }
            }
            self.link(false);
            // Сеанс, в котором была связь, сбрасывает паузу до минимальной.
            if self.session_had_link {
                backoff = Duration::from_secs(1);
            }
            let deadline = Instant::now() + backoff;
            while Instant::now() < deadline && !self.stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100));
            }
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
        tracing::info!("collector остановлен");
    }
}

/// Запускает collector в отдельном OS-потоке.
pub fn spawn(
    cfg: ZmqConfig,
    hub: Arc<LiveHub>,
    writer: mpsc::Sender<WriterMsg>,
    stats: Arc<CollectorStats>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    let c = Collector {
        cfg,
        hub,
        writer,
        stats,
        stop,
        connected: false,
        ever_connected: false,
        session_had_link: false,
    };
    std::thread::Builder::new()
        .name("zmq-collector".into())
        .spawn(move || c.run())
        .expect("не удалось запустить collector")
}

// ---------------------------------------------------------------- record

/// Строка JSONL в формате `docs/research/sample-*.jsonl`.
pub fn record_line(received_at: DateTime<Utc>, bytes: &[u8]) -> (Value, String) {
    let ts = received_at.to_rfc3339_opts(SecondsFormat::Millis, true);
    let text = model::decode_lossy(bytes);
    match serde_json::from_str::<Value>(&text) {
        Ok(v) if v.get("TYPE").and_then(Value::as_str).is_some() => {
            let ty = v["TYPE"].as_str().unwrap_or_default().to_string();
            let data = v.get("DATA").cloned().unwrap_or(Value::Null);
            (json!({"received_at": ts, "type": ty, "data": data}), ty)
        }
        _ => (
            json!({"received_at": ts, "type": model::INVALID_TYPE, "raw": text}),
            model::INVALID_TYPE.to_string(),
        ),
    }
}

/// `qlstats record`: пишет поток в JSONL, БД не трогает.
pub fn record(
    cfg: &ZmqConfig,
    out: &mut dyn Write,
    duration: Option<Duration>,
    stop: Arc<AtomicBool>,
) -> Result<u64> {
    let ctx = zmq::Context::new();
    let (sock, mon) = open_sub(&ctx, cfg, "record")?;
    let start = Instant::now();
    let mut n = 0u64;
    while !stop.load(Ordering::Relaxed) {
        if duration.is_some_and(|d| start.elapsed() >= d) {
            break;
        }
        for (ev, val, addr) in read_monitor(&mon) {
            tracing::info!(event = monitor_event_name(ev), value = val, %addr, "ZMQ monitor");
        }
        match sock.recv_bytes(0) {
            Ok(bytes) => {
                let (line, ty) = record_line(Utc::now(), &bytes);
                writeln!(out, "{line}")?;
                out.flush()?;
                n += 1;
                tracing::info!(kind = %ty, "записано событие");
            }
            Err(zmq::Error::EAGAIN) | Err(zmq::Error::EINTR) => {}
            Err(e) => return Err(anyhow!("ошибка recv: {e}")),
        }
    }
    Ok(n)
}
