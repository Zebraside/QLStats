use chrono::{SecondsFormat, Utc};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn env(k: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| {
        eprintln!("нет переменной окружения {k}");
        std::process::exit(2)
    })
}

fn event_name(e: u16) -> &'static str {
    match e {
        1 => "CONNECTED",
        2 => "CONNECT_DELAYED",
        4 => "CONNECT_RETRIED",
        8 => "LISTENING",
        16 => "BIND_FAILED",
        32 => "ACCEPTED",
        64 => "ACCEPT_FAILED",
        128 => "CLOSED",
        256 => "CLOSE_FAILED",
        512 => "DISCONNECTED",
        1024 => "MONITOR_STOPPED",
        0x0800 => "HANDSHAKE_FAILED_NO_DETAIL",
        0x1000 => "HANDSHAKE_SUCCEEDED",
        0x2000 => "HANDSHAKE_FAILED_PROTOCOL",
        0x4000 => "HANDSHAKE_FAILED_AUTH",
        _ => "UNKNOWN",
    }
}

fn drain_monitor(mon: &zmq::Socket) {
    while let Ok(parts) = mon.recv_multipart(zmq::DONTWAIT) {
        if let Some(f) = parts.first() {
            if f.len() >= 6 {
                let ev = u16::from_le_bytes([f[0], f[1]]);
                let val = u32::from_le_bytes([f[2], f[3], f[4], f[5]]);
                let addr = parts.get(1).map(|a| String::from_utf8_lossy(a).to_string()).unwrap_or_default();
                eprintln!("[monitor] {} (value={}) {}", event_name(ev), val, addr);
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut out = None;
    let mut dur: Option<u64> = None;
    let mut envf: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => { i += 1; out = args.get(i).cloned(); }
            "--duration-secs" => { i += 1; dur = args.get(i).and_then(|s| s.parse().ok()); }
            "--env" => { i += 1; envf = args.get(i).cloned(); }
            _ => { eprintln!("usage: zmq-record --out <file.jsonl> [--duration-secs N] [--env <path/.env>]"); std::process::exit(2); }
        }
        i += 1;
    }
    let out = out.unwrap_or_else(|| { eprintln!("нужен --out"); std::process::exit(2) });
    match envf {
        Some(p) => { dotenvy::from_path(&p).ok(); }
        None => { if dotenvy::from_path("../../.env").is_err() { dotenvy::dotenv().ok(); } }
    }
    let host = env("QL_ZMQ_HOST");
    let port = env("QL_ZMQ_PORT");
    let user = env("QL_ZMQ_USER");
    let pass = env("QL_ZMQ_PASSWORD");

    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    ctrlc::set_handler(move || s2.store(true, Ordering::SeqCst)).unwrap();

    let ctx = zmq::Context::new();
    let sock = ctx.socket(zmq::SUB).unwrap();
    sock.set_plain_username(Some(&user)).unwrap();
    sock.set_plain_password(Some(&pass)).unwrap();
    sock.set_reconnect_ivl(1000).unwrap();
    sock.set_reconnect_ivl_max(10000).unwrap();
    sock.set_rcvtimeo(5000).unwrap();
    sock.set_linger(0).unwrap();
    sock.set_subscribe(b"").unwrap();
    sock.monitor("inproc://mon", zmq::SocketEvent::ALL as i32).unwrap();
    let mon = ctx.socket(zmq::PAIR).unwrap();
    mon.connect("inproc://mon").unwrap();

    let ep = format!("tcp://{host}:{port}");
    eprintln!("подключение к {ep} как {user}");
    sock.connect(&ep).unwrap();

    let mut f = File::create(&out).expect("не открыть файл");
    let start = Instant::now();
    let mut stats: BTreeMap<String, u64> = BTreeMap::new();
    while !stop.load(Ordering::SeqCst) {
        if let Some(d) = dur { if start.elapsed() >= Duration::from_secs(d) { break; } }
        drain_monitor(&mon);
        let bytes = match sock.recv_bytes(0) {
            Ok(b) => b,
            Err(zmq::Error::EAGAIN) | Err(zmq::Error::EINTR) => continue,
            Err(e) => { eprintln!("recv error: {e}"); break; }
        };
        let ts = now();
        let text = String::from_utf8_lossy(&bytes).to_string();
        let (line, ty) = match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => {
                let ty = v.get("TYPE").and_then(|t| t.as_str()).unwrap_or("?").to_string();
                let data = v.get("DATA").cloned().unwrap_or(Value::Null);
                let guid = data.get("MATCH_GUID").and_then(|g| g.as_str());
                let warm = data.get("WARMUP");
                eprintln!("{} {} {} {}", ts, ty,
                    guid.map(|g| format!("guid={g}")).unwrap_or_default(),
                    warm.map(|w| format!("warmup={w}")).unwrap_or_default());
                (json!({"received_at": ts, "type": ty, "data": data}), ty)
            }
            Err(_) => {
                eprintln!("{ts} <не JSON, {} байт>", bytes.len());
                (json!({"received_at": ts, "raw": text}), "<raw>".to_string())
            }
        };
        writeln!(f, "{}", line).unwrap();
        f.flush().unwrap();
        *stats.entry(ty).or_default() += 1;
    }
    drain_monitor(&mon);
    eprintln!("--- статистика за {:.1}s ---", start.elapsed().as_secs_f32());
    for (k, v) in &stats { eprintln!("{k}: {v}"); }
    eprintln!("всего: {}", stats.values().sum::<u64>());
}
