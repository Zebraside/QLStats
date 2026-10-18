//! Прогон записанных JSONL-событий: через writer-пайплайн и `LiveState`, либо как фейковый PUB.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use rusqlite::Connection;
use serde_json::{Value, json};

use crate::config::Secret;
use crate::live::LiveState;
use crate::model;
use crate::store::{self, Writer};
use crate::web;

/// Одна строка записи: время получения и исходный фрейм.
#[derive(Debug, Clone)]
pub struct Frame {
    pub received_at: DateTime<Utc>,
    pub bytes: Vec<u8>,
}

/// Строка `{"received_at", "type", "data"}` (или `"raw"`) → фрейм `{"TYPE","DATA"}`.
pub fn line_to_frame(line: &str) -> Result<Frame> {
    let v: Value = serde_json::from_str(line).context("строка JSONL не разобрана")?;
    let ts = v
        .get("received_at")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("нет received_at"))?;
    let received_at = DateTime::parse_from_rfc3339(ts)
        .with_context(|| format!("некорректный received_at {ts}"))?
        .with_timezone(&Utc);
    let bytes = if let Some(raw) = v.get("raw").and_then(Value::as_str) {
        raw.as_bytes().to_vec()
    } else {
        let ty = v.get("type").cloned().unwrap_or(Value::Null);
        let data = v.get("data").cloned().unwrap_or(Value::Null);
        serde_json::to_vec(&json!({"TYPE": ty, "DATA": data}))?
    };
    Ok(Frame { received_at, bytes })
}

pub fn read_jsonl(path: &Path) -> Result<Vec<Frame>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("не прочитать {}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| line_to_frame(l).with_context(|| format!("строка {}", i + 1)))
        .collect()
}

/// Прогоняет события через writer и live. Возвращает время последнего события.
pub fn run(frames: &[Frame], writer: &mut Writer, live: &mut LiveState) -> Option<DateTime<Utc>> {
    if let Some(first) = frames.first() {
        live.set_link(true, first.received_at);
    }
    for f in frames {
        if let Ok(p) = model::parse(&model::decode_lossy(&f.bytes)) {
            live.apply(&p.event, f.received_at);
        }
        if let Err(e) = writer.process_frame(f.received_at.timestamp_millis(), &f.bytes) {
            tracing::error!(error = %format!("{e:#}"), "replay: ошибка обработки");
        }
    }
    let last = frames.last().map(|f| f.received_at);
    if let Some(t) = last {
        live.tick(t);
    }
    last
}

/// JSON для `--dump`: ответы API и список матчей; часы — `now`.
pub fn dump(
    conn: &Connection,
    live: &LiveState,
    now: DateTime<Utc>,
    tz: Tz,
    offline_secs: u64,
) -> Result<Value> {
    Ok(json!({
        "summary": web::summary_json(conn, tz, now)?,
        "players": web::players_json(conn, tz, now)?,
        "live": live.view(now, offline_secs, tz),
        "matches": store::list_matches(conn)?,
    }))
}

// ---------------------------------------------------------------- publish

const ZAP_ENDPOINT: &str = "inproc://zeromq.zap.01";

/// Простой ZAP-обработчик для PLAIN: проверяет пароль, если он задан.
fn spawn_zap(
    ctx: &zmq::Context,
    password: Option<Secret>,
    stop: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<()>> {
    let sock = ctx.socket(zmq::REP)?;
    sock.bind(ZAP_ENDPOINT)?;
    sock.set_rcvtimeo(500)?;
    Ok(std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            let Ok(frames) = sock.recv_multipart(0) else {
                continue;
            };
            let get = |i: usize| frames.get(i).cloned().unwrap_or_default();
            let mechanism = String::from_utf8_lossy(&get(5)).into_owned();
            let user = String::from_utf8_lossy(&get(6)).into_owned();
            let pass = String::from_utf8_lossy(&get(7)).into_owned();
            let ok = mechanism == "PLAIN" && password.as_ref().is_none_or(|p| p.expose() == pass);
            tracing::info!(%user, ok, "ZAP: запрос аутентификации");
            let (code, text): (&[u8], &[u8]) = if ok {
                (b"200", b"OK")
            } else {
                (b"400", b"Bad credentials")
            };
            let reply: Vec<Vec<u8>> = vec![
                get(0),
                get(1),
                code.to_vec(),
                text.to_vec(),
                user.into_bytes(),
                Vec::new(),
            ];
            let _ = sock.send_multipart(reply, 0);
        }
    }))
}

/// `replay --publish`: фейковый QL-сервер (PUB + PLAIN), исходные интервалы делятся на `speed`.
pub fn publish(
    frames: &[Frame],
    endpoint: &str,
    speed: f64,
    looped: bool,
    password: Option<Secret>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let ctx = zmq::Context::new();
    let zap = spawn_zap(&ctx, password, stop.clone())?;
    let sock = ctx.socket(zmq::PUB)?;
    sock.set_plain_server(true)?;
    sock.set_zap_domain("stats")?;
    sock.set_linger(0)?;
    sock.bind(endpoint)
        .with_context(|| format!("bind {endpoint}"))?;
    tracing::info!(%endpoint, "replay: PUB запущен, ожидание подписчиков 1 с");
    std::thread::sleep(Duration::from_secs(1));
    let speed = if speed > 0.0 { speed } else { 1.0 };
    'outer: loop {
        let start = Instant::now();
        let t0 = frames.first().map(|f| f.received_at);
        for f in frames {
            if stop.load(Ordering::Relaxed) {
                break 'outer;
            }
            if let Some(t0) = t0 {
                let offset = (f.received_at - t0).to_std().unwrap_or_default();
                let due = start + offset.div_f64(speed);
                while Instant::now() < due {
                    if stop.load(Ordering::Relaxed) {
                        break 'outer;
                    }
                    std::thread::sleep((due - Instant::now()).min(Duration::from_millis(100)));
                }
            }
            sock.send(&f.bytes, 0)?;
        }
        tracing::info!(events = frames.len(), "replay: файл отправлен");
        if !looped {
            break;
        }
    }
    stop.store(true, Ordering::Relaxed);
    let _ = zap.join();
    Ok(())
}
