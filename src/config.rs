//! Конфигурация из переменных окружения и `.env`.

use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use chrono_tz::Tz;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("не задана переменная окружения {0}")]
    Missing(&'static str),
    #[error("некорректное значение {name}: {value}")]
    Invalid { name: &'static str, value: String },
}

/// Пароль, который не попадает ни в `Debug`, ни в `Display`.
#[derive(Clone, Default)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("\"***\"")
    }
}

/// Параметры подключения к ZMQ.
#[derive(Debug, Clone)]
pub struct ZmqConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: Secret,
}

impl ZmqConfig {
    pub fn endpoint(&self) -> String {
        format!("tcp://{}:{}", self.host, self.port)
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    /// `None`, если `QL_ZMQ_HOST` не задан: нужен только для `run` и `record`.
    pub zmq: Option<ZmqConfig>,
    pub db_path: PathBuf,
    pub http_addr: SocketAddr,
    pub tz: Tz,
    pub live_offline_secs: u64,
    pub static_dir: Option<PathBuf>,
}

/// Загружает `.env` (явный путь или `./.env`, если он есть). Уже заданные
/// переменные окружения имеют приоритет.
pub fn load_env_file(path: Option<&Path>) -> Result<(), ConfigError> {
    match path {
        // Текст строки из ошибки разбора не выводим: там может быть пароль.
        Some(p) => dotenvy::from_path(p).map_err(|e| ConfigError::Invalid {
            name: "--env-file",
            value: match e {
                dotenvy::Error::LineParse(_, idx) => {
                    format!("{}: ошибка разбора (позиция {idx})", p.display())
                }
                dotenvy::Error::Io(io) => format!("{}: {io}", p.display()),
                _ => format!("{}: ошибка загрузки", p.display()),
            },
        }),
        None => {
            let _ = dotenvy::dotenv();
            Ok(())
        }
    }
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let zmq = match var("QL_ZMQ_HOST") {
            None => None,
            Some(host) => {
                let port_s = var("QL_ZMQ_PORT").ok_or(ConfigError::Missing("QL_ZMQ_PORT"))?;
                let port = port_s.parse().map_err(|_| ConfigError::Invalid {
                    name: "QL_ZMQ_PORT",
                    value: port_s,
                })?;
                let password =
                    var("QL_ZMQ_PASSWORD").ok_or(ConfigError::Missing("QL_ZMQ_PASSWORD"))?;
                Some(ZmqConfig {
                    host,
                    port,
                    user: var("QL_ZMQ_USER").unwrap_or_else(|| "stats".into()),
                    password: Secret::new(password),
                })
            }
        };
        let db_path = var("QLSTATS_DB_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("./data/qlstats.db"));
        let addr_s = var("QLSTATS_HTTP_ADDR").unwrap_or_else(|| "0.0.0.0:8080".into());
        let http_addr = addr_s.parse().map_err(|_| ConfigError::Invalid {
            name: "QLSTATS_HTTP_ADDR",
            value: addr_s,
        })?;
        let tz_s = var("QLSTATS_TZ").unwrap_or_else(|| "Europe/Moscow".into());
        let tz = tz_s.parse().map_err(|_| ConfigError::Invalid {
            name: "QLSTATS_TZ",
            value: tz_s,
        })?;
        let live_offline_secs = match var("QLSTATS_LIVE_OFFLINE_SECS") {
            None => 15,
            Some(s) => s.parse().map_err(|_| ConfigError::Invalid {
                name: "QLSTATS_LIVE_OFFLINE_SECS",
                value: s,
            })?,
        };
        Ok(Self {
            zmq,
            db_path,
            http_addr,
            tz,
            live_offline_secs,
            static_dir: var("QLSTATS_STATIC_DIR").map(PathBuf::from),
        })
    }

    pub fn require_zmq(&self) -> Result<&ZmqConfig, ConfigError> {
        self.zmq.as_ref().ok_or(ConfigError::Missing("QL_ZMQ_HOST"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_is_masked() {
        let z = ZmqConfig {
            host: "h".into(),
            port: 1,
            user: "stats".into(),
            password: Secret::new("topsecret"),
        };
        let s = format!("{z:?}");
        assert!(!s.contains("topsecret"));
        assert!(s.contains("***"));
    }
}
