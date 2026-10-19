//! CLI: `run`, `recompute`, `record`, `replay`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;

use crate::collector::{self, CollectorStats};
use crate::config::{Config, Sources};
use crate::live::{LiveHub, LiveState};
use crate::replay;
use crate::store::{self, Writer};
use crate::web::{self, AppState, SystemClock};

#[derive(Debug, Parser)]
#[command(name = "qlstats", version, about = "Статистика Quake Live по ZMQ")]
pub struct Cli {
    /// TOML-файл конфигурации (по умолчанию не читается) [env: QLSTATS_CONFIG].
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Файл с переменными окружения (по умолчанию ./.env, если есть).
    #[arg(long, global = true, value_name = "PATH")]
    pub env_file: Option<PathBuf>,
    /// Путь к SQLite (перекрывает QLSTATS_DB_PATH и database.path).
    #[arg(long, global = true, value_name = "PATH")]
    pub db: Option<PathBuf>,
    /// Адрес HTTP-сервера (перекрывает QLSTATS_HTTP_ADDR и http.listen).
    #[arg(long, global = true, value_name = "ADDR")]
    pub http_addr: Option<SocketAddr>,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Основной режим: collector, writer и web.
    Run,
    /// Пересоздать нормализованные данные из raw_events (с бэкапом VACUUM INTO).
    Recompute,
    /// Записать поток ZMQ в JSONL (БД не трогает).
    Record {
        #[arg(long)]
        out: PathBuf,
        /// Остановиться через N секунд.
        #[arg(long)]
        duration_secs: Option<u64>,
    },
    /// Прогнать JSONL через пайплайн (по умолчанию во временную БД) или опубликовать как PUB.
    Replay {
        file: PathBuf,
        /// Напечатать JSON summary/players/live/matches после прогона.
        #[arg(long)]
        dump: bool,
        /// Работать как фейковый QL-сервер: ZMQ PUB с PLAIN на этом адресе.
        #[arg(long)]
        publish: Option<String>,
        /// Множитель скорости для --publish.
        #[arg(long, default_value_t = 1.0)]
        speed: f64,
        /// Повторять файл по кругу (для --publish).
        #[arg(long = "loop")]
        looped: bool,
    },
}

fn init_logging(to_stderr: bool) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    use std::io::IsTerminal;
    let b = tracing_subscriber::fmt().with_env_filter(filter);
    if to_stderr {
        b.with_ansi(std::io::stderr().is_terminal())
            .with_writer(std::io::stderr)
            .init();
    } else {
        b.with_ansi(std::io::stdout().is_terminal()).init();
    }
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    let to_stderr = !matches!(cli.cmd, Cmd::Run);
    init_logging(to_stderr);
    match dispatch(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "завершение с ошибкой");
            eprintln!("ошибка: {e:#}");
            ExitCode::FAILURE
        }
    }
}

impl Cli {
    fn sources(&self) -> Sources {
        Sources {
            // Переменная читается вручную, а не через clap `env`: пустое значение
            // (QLSTATS_CONFIG=) должно означать «не задано», как у остальных переменных,
            // чтобы в Docker можно было отключить конфиг, зашитый в образ (ENV).
            config_file: self.config.clone().or_else(|| {
                std::env::var_os("QLSTATS_CONFIG")
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
            }),
            env_file: self.env_file.clone(),
            db: self.db.clone(),
            http_addr: self.http_addr,
        }
    }
}

fn stop_flag() -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    let _ = ctrlc::set_handler(move || s.store(true, Ordering::SeqCst));
    stop
}

fn dispatch(cli: Cli) -> Result<()> {
    let cfg = Config::load(&cli.sources())?;
    match cli.cmd {
        Cmd::Run => run(cfg),
        Cmd::Recompute => recompute(cfg),
        Cmd::Record { out, duration_secs } => {
            let zmq = cfg.require_zmq()?;
            let mut f = std::io::BufWriter::new(
                std::fs::File::create(&out)
                    .with_context(|| format!("не создать {}", out.display()))?,
            );
            let n = collector::record(
                zmq,
                &mut f,
                duration_secs.map(Duration::from_secs),
                stop_flag(),
            )?;
            tracing::info!(events = n, file = %out.display(), "запись завершена");
            Ok(())
        }
        Cmd::Replay {
            file,
            dump,
            publish,
            speed,
            looped,
        } => {
            let frames = replay::read_jsonl(&file)?;
            if let Some(ep) = publish {
                let pass = cfg.zmq.as_ref().map(|z| z.password.clone());
                return replay::publish(&frames, &ep, speed, looped, pass, stop_flag());
            }
            replay_db(&cfg, cli.db.is_some(), &frames, dump)
        }
    }
}

fn replay_db(cfg: &Config, explicit_db: bool, frames: &[replay::Frame], dump: bool) -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let path = if explicit_db {
        cfg.db_path.clone()
    } else {
        tmp.path().join("replay.db")
    };
    let conn = store::open(&path)?;
    let mut writer = Writer::new(conn, cfg.tz, None);
    let first = frames
        .first()
        .map(|f| f.received_at)
        .unwrap_or_else(Utc::now);
    let mut live = LiveState::new(first);
    let last = replay::run(frames, &mut writer, &mut live).unwrap_or(first);
    tracing::info!(events = frames.len(), db = %path.display(), "replay завершён");
    if dump {
        let v = replay::dump(writer.conn(), &live, last, cfg.tz, cfg.live_offline_secs)?;
        println!("{}", serde_json::to_string_pretty(&v)?);
    }
    Ok(())
}

fn recompute(cfg: Config) -> Result<()> {
    let mut conn = store::open(&cfg.db_path)?;
    let backup = PathBuf::from(format!(
        "{}.bak-{}",
        cfg.db_path.display(),
        Utc::now().format("%Y%m%d-%H%M%S")
    ));
    tracing::warn!(
        db = %cfg.db_path.display(),
        "recompute удалит и пересоздаст matches/match_players/deaths/players; лучше запускать при остановленном сервисе"
    );
    store::backup(&conn, &backup)?;
    tracing::info!(backup = %backup.display(), "бэкап создан");
    let sum = store::recompute(&mut conn, cfg.tz)?;
    tracing::info!(
        raw_events = sum.raw_events,
        matches = sum.matches,
        counted = sum.counted,
        "пересчёт завершён"
    );
    println!("{}", serde_json::to_string(&sum)?);
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
    tracing::info!("получен сигнал остановки");
}

fn run(cfg: Config) -> Result<()> {
    let zmq = cfg.require_zmq()?.clone();
    tracing::info!(
        db = %cfg.db_path.display(),
        http = %cfg.http_addr,
        tz = %cfg.tz,
        zmq = %zmq.endpoint(),
        "запуск qlstats"
    );
    let conn = store::open(&cfg.db_path)?;

    // Восстановление live-состояния из raw_events.
    let now = Utc::now();
    let mut live = LiveState::new(now);
    let tail = store::tail_after_last_report(&conn)?;
    live.restore(tail.iter().map(|(_, at, p)| (*at, p.as_str())), now);
    tracing::info!(events = tail.len(), phase = ?live.phase(), "live-состояние восстановлено");
    let hub = Arc::new(LiveHub::new(live, cfg.live_offline_secs, cfg.tz));

    let (data_tx, data_rx) = watch::channel(0u64);
    let (wtx, wrx) = mpsc::channel();
    let writer_h = store::spawn_writer(Writer::new(conn, cfg.tz, Some(data_tx)), wrx);

    let cstats = Arc::new(CollectorStats::default());
    let stop = Arc::new(AtomicBool::new(false));
    let coll_h = collector::spawn(zmq, hub.clone(), wtx, cstats.clone(), stop.clone());

    let read_conn = store::open(&cfg.db_path)?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let res = rt.block_on(async {
        let state = AppState::new(
            read_conn,
            cfg.tz,
            Arc::new(SystemClock),
            hub,
            cstats,
            cfg.static_dir.clone(),
        )?;
        web::spawn_background(state.clone(), data_rx);
        web::serve(state, cfg.http_addr, &cfg.http_limits, shutdown_signal()).await
    });
    stop.store(true, Ordering::SeqCst);
    let _ = coll_h.join();
    let _ = writer_h.join();
    rt.shutdown_timeout(Duration::from_secs(2));
    tracing::info!("qlstats остановлен");
    res
}
