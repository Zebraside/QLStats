//! Конфигурация: TOML-файл, переменные окружения (и `.env`), флаги CLI.
//!
//! Приоритет: флаги CLI > переменные окружения (включая `.env`) > TOML-файл >
//! значения по умолчанию. Каждый источник превращается в [`PartialConfig`]
//! (все поля необязательны), слои накладываются друг на друга, затем
//! [`PartialConfig::resolve`] проверяет значения и даёт [`Config`].

use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use chrono_tz::Tz;
use serde::{Deserialize, Deserializer};
use thiserror::Error;

const DEFAULT_DB_PATH: &str = "./data/qlstats.db";
const DEFAULT_HTTP_ADDR: &str = "0.0.0.0:8080";
const DEFAULT_TZ: &str = "Europe/Moscow";
const DEFAULT_ZMQ_USER: &str = "stats";
const DEFAULT_LIVE_OFFLINE_SECS: u64 = 15;
const DEFAULT_RATE_LIMIT_PER_SEC: u64 = 10;
const DEFAULT_RATE_LIMIT_BURST: u32 = 60;
const DEFAULT_MAX_CONCURRENCY: usize = 256;
/// Верхняя граница `http.max_concurrency` (больше не имеет смысла).
const MAX_CONCURRENCY_LIMIT: usize = 1_000_000;
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 10;

#[derive(Debug, Error)]
pub enum ConfigError {
    /// `key` — ключ TOML, `env` — переменная окружения.
    #[error("не задан {key} ({env})")]
    Missing {
        key: &'static str,
        env: &'static str,
    },
    /// `name` — источник значения: переменная окружения, ключ TOML или флаг.
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

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(Secret)
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

/// Защита HTTP-сервера от флуда (секция `[http]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpLimits {
    /// Сколько запросов в секунду восстанавливается в per-IP квоте; `0` — per-IP лимит выключен.
    pub rate_limit_per_sec: u64,
    /// Ёмкость per-IP квоты (сколько запросов можно сделать подряд); не меньше 1.
    pub rate_limit_burst: u32,
    /// Предел одновременных запросов на весь сервер; `0` — без предела.
    pub max_concurrency: usize,
    /// Брать IP клиента из `X-Forwarded-For`/`X-Real-IP`/`Forwarded` (только за прокси!).
    pub trust_proxy: bool,
    /// Таймаут обработки запроса, секунды; `0` — без таймаута.
    pub request_timeout_secs: u64,
}

impl Default for HttpLimits {
    fn default() -> Self {
        Self {
            rate_limit_per_sec: DEFAULT_RATE_LIMIT_PER_SEC,
            rate_limit_burst: DEFAULT_RATE_LIMIT_BURST,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            trust_proxy: false,
            request_timeout_secs: DEFAULT_REQUEST_TIMEOUT_SECS,
        }
    }
}

impl HttpLimits {
    /// Все ограничения выключены (тесты, отладка).
    pub fn disabled() -> Self {
        Self {
            rate_limit_per_sec: 0,
            rate_limit_burst: DEFAULT_RATE_LIMIT_BURST,
            max_concurrency: 0,
            trust_proxy: false,
            request_timeout_secs: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    /// `None`, если `zmq.host`/`QL_ZMQ_HOST` не задан: нужен только для `run` и `record`.
    pub zmq: Option<ZmqConfig>,
    pub db_path: PathBuf,
    pub http_addr: SocketAddr,
    pub tz: Tz,
    pub live_offline_secs: u64,
    pub static_dir: Option<PathBuf>,
    pub http_limits: HttpLimits,
}

/// Значения, заданные флагами CLI, и пути к файлам конфигурации.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    /// `--config` / `QLSTATS_CONFIG`: TOML-файл. `None` — файл не читается.
    pub config_file: Option<PathBuf>,
    /// `--env-file`: `None` — `./.env`, если он есть.
    pub env_file: Option<PathBuf>,
    /// `--db`.
    pub db: Option<PathBuf>,
    /// `--http-addr`.
    pub http_addr: Option<SocketAddr>,
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

// --- TOML-файл -------------------------------------------------------------

/// Содержимое TOML-файла. Все секции и поля необязательны.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileConfig {
    pub database: FileDatabase,
    pub zmq: FileZmq,
    pub http: FileHttp,
    pub general: FileGeneral,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileDatabase {
    pub path: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileZmq {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub password: Option<Secret>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileHttp {
    pub listen: Option<String>,
    pub rate_limit_per_sec: Option<u64>,
    pub rate_limit_burst: Option<u32>,
    pub max_concurrency: Option<usize>,
    pub trust_proxy: Option<bool>,
    pub request_timeout_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileGeneral {
    pub tz: Option<String>,
    pub live_offline_secs: Option<u64>,
    pub static_dir: Option<PathBuf>,
}

impl FileConfig {
    /// Разбирает TOML. В ошибке — путь, строка/столбец и сообщение парсера, из
    /// которого вырезаны значения в кавычках: там может оказаться пароль.
    pub fn parse(text: &str, path: &Path) -> Result<Self, ConfigError> {
        toml::from_str(text).map_err(|e| {
            let pos = match e.span() {
                Some(span) => {
                    let (line, col) = line_col(text, span.start);
                    format!("строка {line}, столбец {col}")
                }
                None => "позиция неизвестна".into(),
            };
            ConfigError::Invalid {
                name: "--config",
                value: format!(
                    "{}: ошибка разбора ({pos}): {}",
                    path.display(),
                    redact_quoted(e.message().trim())
                ),
            }
        })
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Invalid {
            name: "--config",
            value: format!("{}: {e}", path.display()),
        })?;
        Self::parse(&text, path)
    }
}

/// Номер строки и столбца (с 1, столбец в символах) для байтового смещения.
fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..text.floor_char_boundary(offset.min(text.len()))];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    (line, col)
}

/// Заменяет на `…` содержимое `"…"`, `'…'` и `` `…` `` в сообщении парсера
/// (serde цитирует так значения: `invalid type: string "…"`). Сообщение о
/// неизвестном ключе цитирует только имена ключей — его оставляем как есть.
fn redact_quoted(msg: &str) -> String {
    if msg.starts_with("unknown field `") {
        return msg.to_owned();
    }
    let mut out = String::with_capacity(msg.len());
    let mut close: Option<char> = None;
    for c in msg.chars() {
        match close {
            Some(q) if c == q => {
                out.push('…');
                out.push(c);
                close = None;
            }
            Some(_) => {}
            None => {
                out.push(c);
                if matches!(c, '"' | '\'' | '`') {
                    close = Some(c);
                }
            }
        }
    }
    if close.is_some() {
        out.push('…');
    }
    out
}

// --- Слои ------------------------------------------------------------------

/// Строковое значение вместе с именем источника (для текста ошибки).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sourced {
    value: String,
    source: &'static str,
}

fn sourced(value: Option<String>, source: &'static str) -> Option<Sourced> {
    value
        .filter(|v| !v.trim().is_empty())
        .map(|value| Sourced { value, source })
}

/// Типизированное значение из TOML как [`Sourced`] (проверяется в `resolve`).
fn sourced_val<T: ToString>(value: Option<T>, source: &'static str) -> Option<Sourced> {
    value.map(|v| Sourced {
        value: v.to_string(),
        source,
    })
}

/// Один слой конфигурации: всё, что задал один источник.
#[derive(Debug, Clone, Default)]
pub struct PartialConfig {
    zmq_host: Option<String>,
    zmq_port: Option<Sourced>,
    zmq_user: Option<String>,
    zmq_password: Option<Secret>,
    db_path: Option<PathBuf>,
    http_addr: Option<Sourced>,
    tz: Option<Sourced>,
    live_offline_secs: Option<Sourced>,
    static_dir: Option<PathBuf>,
    rate_limit_per_sec: Option<Sourced>,
    rate_limit_burst: Option<Sourced>,
    max_concurrency: Option<Sourced>,
    trust_proxy: Option<Sourced>,
    request_timeout_secs: Option<Sourced>,
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|v| !v.trim().is_empty())
}

fn non_empty_path(p: Option<PathBuf>) -> Option<PathBuf> {
    p.filter(|p| !p.as_os_str().is_empty())
}

impl PartialConfig {
    pub fn from_file(f: FileConfig) -> Self {
        Self {
            zmq_host: non_empty(f.zmq.host),
            zmq_port: f.zmq.port.map(|p| Sourced {
                value: p.to_string(),
                source: "zmq.port",
            }),
            zmq_user: non_empty(f.zmq.user),
            zmq_password: f.zmq.password.filter(|s| !s.expose().trim().is_empty()),
            db_path: non_empty_path(f.database.path),
            http_addr: sourced(f.http.listen, "http.listen"),
            tz: sourced(f.general.tz, "general.tz"),
            live_offline_secs: f.general.live_offline_secs.map(|n| Sourced {
                value: n.to_string(),
                source: "general.live_offline_secs",
            }),
            static_dir: non_empty_path(f.general.static_dir),
            rate_limit_per_sec: sourced_val(f.http.rate_limit_per_sec, "http.rate_limit_per_sec"),
            rate_limit_burst: sourced_val(f.http.rate_limit_burst, "http.rate_limit_burst"),
            max_concurrency: sourced_val(f.http.max_concurrency, "http.max_concurrency"),
            trust_proxy: sourced_val(f.http.trust_proxy, "http.trust_proxy"),
            request_timeout_secs: sourced_val(
                f.http.request_timeout_secs,
                "http.request_timeout_secs",
            ),
        }
    }

    /// Слой из переменных окружения; `get` — источник (в тестах — не реальный env).
    /// Пустые значения считаются незаданными.
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Self {
        Self {
            zmq_host: non_empty(get("QL_ZMQ_HOST")),
            zmq_port: sourced(get("QL_ZMQ_PORT"), "QL_ZMQ_PORT"),
            zmq_user: non_empty(get("QL_ZMQ_USER")),
            zmq_password: non_empty(get("QL_ZMQ_PASSWORD")).map(Secret::new),
            db_path: non_empty(get("QLSTATS_DB_PATH")).map(PathBuf::from),
            http_addr: sourced(get("QLSTATS_HTTP_ADDR"), "QLSTATS_HTTP_ADDR"),
            tz: sourced(get("QLSTATS_TZ"), "QLSTATS_TZ"),
            live_offline_secs: sourced(
                get("QLSTATS_LIVE_OFFLINE_SECS"),
                "QLSTATS_LIVE_OFFLINE_SECS",
            ),
            static_dir: non_empty(get("QLSTATS_STATIC_DIR")).map(PathBuf::from),
            rate_limit_per_sec: sourced(
                get("QLSTATS_RATE_LIMIT_PER_SEC"),
                "QLSTATS_RATE_LIMIT_PER_SEC",
            ),
            rate_limit_burst: sourced(get("QLSTATS_RATE_LIMIT_BURST"), "QLSTATS_RATE_LIMIT_BURST"),
            max_concurrency: sourced(get("QLSTATS_MAX_CONCURRENCY"), "QLSTATS_MAX_CONCURRENCY"),
            trust_proxy: sourced(get("QLSTATS_TRUST_PROXY"), "QLSTATS_TRUST_PROXY"),
            request_timeout_secs: sourced(
                get("QLSTATS_REQUEST_TIMEOUT_SECS"),
                "QLSTATS_REQUEST_TIMEOUT_SECS",
            ),
        }
    }

    pub fn from_cli(src: &Sources) -> Self {
        Self {
            db_path: src.db.clone(),
            http_addr: src.http_addr.map(|a| Sourced {
                value: a.to_string(),
                source: "--http-addr",
            }),
            ..Self::default()
        }
    }

    /// Накладывает `top` поверх `self`: заданные в `top` поля побеждают.
    pub fn overlay(self, top: Self) -> Self {
        Self {
            zmq_host: top.zmq_host.or(self.zmq_host),
            zmq_port: top.zmq_port.or(self.zmq_port),
            zmq_user: top.zmq_user.or(self.zmq_user),
            zmq_password: top.zmq_password.or(self.zmq_password),
            db_path: top.db_path.or(self.db_path),
            http_addr: top.http_addr.or(self.http_addr),
            tz: top.tz.or(self.tz),
            live_offline_secs: top.live_offline_secs.or(self.live_offline_secs),
            static_dir: top.static_dir.or(self.static_dir),
            rate_limit_per_sec: top.rate_limit_per_sec.or(self.rate_limit_per_sec),
            rate_limit_burst: top.rate_limit_burst.or(self.rate_limit_burst),
            max_concurrency: top.max_concurrency.or(self.max_concurrency),
            trust_proxy: top.trust_proxy.or(self.trust_proxy),
            request_timeout_secs: top.request_timeout_secs.or(self.request_timeout_secs),
        }
    }

    /// Проверяет значения, подставляет умолчания.
    pub fn resolve(self) -> Result<Config, ConfigError> {
        let zmq = match self.zmq_host {
            None => None,
            Some(host) => {
                let port = self.zmq_port.ok_or(ConfigError::Missing {
                    key: "zmq.port",
                    env: "QL_ZMQ_PORT",
                })?;
                let password = self.zmq_password.ok_or(ConfigError::Missing {
                    key: "zmq.password",
                    env: "QL_ZMQ_PASSWORD",
                })?;
                Some(ZmqConfig {
                    host,
                    port: parse(port)?,
                    user: self.zmq_user.unwrap_or_else(|| DEFAULT_ZMQ_USER.into()),
                    password,
                })
            }
        };
        let d = HttpLimits::default();
        let http_limits = HttpLimits {
            rate_limit_per_sec: parse_or(self.rate_limit_per_sec, d.rate_limit_per_sec)?,
            rate_limit_burst: match self.rate_limit_burst {
                // governor не принимает нулевую ёмкость.
                Some(s) => match parse::<u32>(s.clone())? {
                    0 => {
                        return Err(ConfigError::Invalid {
                            name: s.source,
                            value: s.value,
                        });
                    }
                    n => n,
                },
                None => d.rate_limit_burst,
            },
            max_concurrency: match self.max_concurrency {
                // Семафор tokio паникует на огромном числе разрешений; ловим здесь.
                Some(s) => match parse::<usize>(s.clone())? {
                    n if n > MAX_CONCURRENCY_LIMIT => {
                        return Err(ConfigError::Invalid {
                            name: s.source,
                            value: s.value,
                        });
                    }
                    n => n,
                },
                None => d.max_concurrency,
            },
            trust_proxy: match self.trust_proxy {
                Some(s) => parse_bool(s)?,
                None => d.trust_proxy,
            },
            request_timeout_secs: parse_or(self.request_timeout_secs, d.request_timeout_secs)?,
        };
        Ok(Config {
            zmq,
            db_path: self
                .db_path
                .unwrap_or_else(|| PathBuf::from(DEFAULT_DB_PATH)),
            http_addr: match self.http_addr {
                Some(s) => parse(s)?,
                None => DEFAULT_HTTP_ADDR.parse().expect("корректный адрес"),
            },
            tz: match self.tz {
                Some(s) => parse(s)?,
                None => DEFAULT_TZ.parse().expect("корректный часовой пояс"),
            },
            live_offline_secs: match self.live_offline_secs {
                Some(s) => parse(s)?,
                None => DEFAULT_LIVE_OFFLINE_SECS,
            },
            static_dir: self.static_dir,
            http_limits,
        })
    }
}

fn parse<T: std::str::FromStr>(s: Sourced) -> Result<T, ConfigError> {
    s.value.trim().parse().map_err(|_| ConfigError::Invalid {
        name: s.source,
        value: s.value,
    })
}

fn parse_or<T: std::str::FromStr>(s: Option<Sourced>, default: T) -> Result<T, ConfigError> {
    s.map_or(Ok(default), parse)
}

/// `true/false`, `1/0`, `yes/no`, `on/off` без учёта регистра.
fn parse_bool(s: Sourced) -> Result<bool, ConfigError> {
    match s.value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(ConfigError::Invalid {
            name: s.source,
            value: s.value,
        }),
    }
}

impl Config {
    /// Загружает `.env`, затем собирает конфигурацию из TOML, окружения процесса и флагов.
    pub fn load(src: &Sources) -> Result<Self, ConfigError> {
        load_env_file(src.env_file.as_deref())?;
        Self::build(src, |k| std::env::var(k).ok())
    }

    /// Как [`Config::load`], но без `.env` и с явным источником переменных окружения.
    pub fn build(src: &Sources, env: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let file = match &src.config_file {
            Some(path) => PartialConfig::from_file(FileConfig::load(path)?),
            None => PartialConfig::default(),
        };
        file.overlay(PartialConfig::from_env(env))
            .overlay(PartialConfig::from_cli(src))
            .resolve()
    }

    pub fn require_zmq(&self) -> Result<&ZmqConfig, ConfigError> {
        self.zmq.as_ref().ok_or(ConfigError::Missing {
            key: "zmq.host",
            env: "QL_ZMQ_HOST",
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::Write;

    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| m.get(k).cloned()
    }

    fn toml_file(text: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(text.as_bytes()).unwrap();
        f
    }

    fn sources(file: Option<&tempfile::NamedTempFile>) -> Sources {
        Sources {
            config_file: file.map(|f| f.path().to_path_buf()),
            ..Sources::default()
        }
    }

    const FULL: &str = r#"
[database]
path = "/toml/db.sqlite"

[zmq]
host = "1.2.3.4"
port = 27960
user = "tomluser"
password = "tomlsecret"

[http]
listen = "127.0.0.1:1111"

[general]
tz = "UTC"
live_offline_secs = 30
static_dir = "static"
"#;

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

        let f =
            FileConfig::parse("[zmq]\npassword = \"topsecret\"\n", Path::new("c.toml")).unwrap();
        assert_eq!(f.zmq.password.as_ref().unwrap().expose(), "topsecret");
        assert!(!format!("{f:?}").contains("topsecret"));
        let p = PartialConfig::from_file(f);
        assert!(!format!("{p:?}").contains("topsecret"));
    }

    #[test]
    fn defaults_without_any_source() {
        let cfg = Config::build(&Sources::default(), env(&[])).unwrap();
        assert!(cfg.zmq.is_none());
        assert_eq!(cfg.db_path, PathBuf::from(DEFAULT_DB_PATH));
        assert_eq!(cfg.http_addr, "0.0.0.0:8080".parse().unwrap());
        assert_eq!(cfg.tz, chrono_tz::Europe::Moscow);
        assert_eq!(cfg.live_offline_secs, 15);
        assert!(cfg.static_dir.is_none());
        assert_eq!(
            cfg.http_limits,
            HttpLimits {
                rate_limit_per_sec: 10,
                rate_limit_burst: 60,
                max_concurrency: 256,
                trust_proxy: false,
                request_timeout_secs: 10,
            }
        );
    }

    #[test]
    fn toml_overrides_defaults() {
        let f = toml_file(FULL);
        let cfg = Config::build(&sources(Some(&f)), env(&[])).unwrap();
        let z = cfg.zmq.as_ref().unwrap();
        assert_eq!(z.endpoint(), "tcp://1.2.3.4:27960");
        assert_eq!(z.user, "tomluser");
        assert_eq!(z.password.expose(), "tomlsecret");
        assert_eq!(cfg.db_path, PathBuf::from("/toml/db.sqlite"));
        assert_eq!(cfg.http_addr, "127.0.0.1:1111".parse().unwrap());
        assert_eq!(cfg.tz, chrono_tz::UTC);
        assert_eq!(cfg.live_offline_secs, 30);
        assert_eq!(cfg.static_dir, Some(PathBuf::from("static")));
    }

    #[test]
    fn priority_cli_over_env_over_toml_over_default() {
        let f = toml_file(FULL);
        let e = env(&[
            ("QLSTATS_DB_PATH", "/env/db.sqlite"),
            ("QLSTATS_HTTP_ADDR", "127.0.0.1:2222"),
            ("QLSTATS_TZ", "Europe/Berlin"),
            ("QL_ZMQ_PASSWORD", "envsecret"),
        ]);

        // env > TOML; поля, которых нет в env, берутся из TOML.
        let cfg = Config::build(&sources(Some(&f)), &e).unwrap();
        assert_eq!(cfg.db_path, PathBuf::from("/env/db.sqlite"));
        assert_eq!(cfg.http_addr, "127.0.0.1:2222".parse().unwrap());
        assert_eq!(cfg.tz, chrono_tz::Europe::Berlin);
        assert_eq!(cfg.live_offline_secs, 30);
        let z = cfg.zmq.as_ref().unwrap();
        assert_eq!(z.password.expose(), "envsecret");
        assert_eq!(z.user, "tomluser");

        // CLI > env > TOML.
        let src = Sources {
            db: Some("/cli/db.sqlite".into()),
            http_addr: Some("127.0.0.1:3333".parse().unwrap()),
            ..sources(Some(&f))
        };
        let cfg = Config::build(&src, &e).unwrap();
        assert_eq!(cfg.db_path, PathBuf::from("/cli/db.sqlite"));
        assert_eq!(cfg.http_addr, "127.0.0.1:3333".parse().unwrap());
        assert_eq!(cfg.tz, chrono_tz::Europe::Berlin);

        // CLI без TOML и env.
        let src = Sources {
            http_addr: Some("127.0.0.1:4444".parse().unwrap()),
            ..Sources::default()
        };
        let cfg = Config::build(&src, env(&[])).unwrap();
        assert_eq!(cfg.http_addr, "127.0.0.1:4444".parse().unwrap());
        assert_eq!(cfg.db_path, PathBuf::from(DEFAULT_DB_PATH));
    }

    #[test]
    fn http_limits_priority_env_over_toml() {
        let f = toml_file(
            "[http]\nrate_limit_per_sec = 5\nrate_limit_burst = 20\nmax_concurrency = 64\n\
             trust_proxy = true\nrequest_timeout_secs = 3\n",
        );
        let cfg = Config::build(&sources(Some(&f)), env(&[])).unwrap();
        assert_eq!(
            cfg.http_limits,
            HttpLimits {
                rate_limit_per_sec: 5,
                rate_limit_burst: 20,
                max_concurrency: 64,
                trust_proxy: true,
                request_timeout_secs: 3,
            }
        );

        // env > TOML; незаданные в env поля — из TOML.
        let e = env(&[
            ("QLSTATS_RATE_LIMIT_PER_SEC", "50"),
            ("QLSTATS_MAX_CONCURRENCY", "1000"),
            ("QLSTATS_TRUST_PROXY", "false"),
        ]);
        let cfg = Config::build(&sources(Some(&f)), &e).unwrap();
        assert_eq!(cfg.http_limits.rate_limit_per_sec, 50);
        assert_eq!(cfg.http_limits.rate_limit_burst, 20);
        assert_eq!(cfg.http_limits.max_concurrency, 1000);
        assert!(!cfg.http_limits.trust_proxy);
        assert_eq!(cfg.http_limits.request_timeout_secs, 3);

        // env без TOML: остальное — умолчания.
        let e = env(&[
            ("QLSTATS_RATE_LIMIT_BURST", "7"),
            ("QLSTATS_TRUST_PROXY", "1"),
            ("QLSTATS_REQUEST_TIMEOUT_SECS", "30"),
        ]);
        let cfg = Config::build(&Sources::default(), &e).unwrap();
        assert_eq!(cfg.http_limits.rate_limit_per_sec, 10);
        assert_eq!(cfg.http_limits.rate_limit_burst, 7);
        assert!(cfg.http_limits.trust_proxy);
        assert_eq!(cfg.http_limits.request_timeout_secs, 30);
    }

    #[test]
    fn http_limits_zero_disables() {
        let f = toml_file("[http]\nrate_limit_per_sec = 0\nmax_concurrency = 0\n");
        let cfg = Config::build(&sources(Some(&f)), env(&[])).unwrap();
        assert_eq!(cfg.http_limits.rate_limit_per_sec, 0);
        assert_eq!(cfg.http_limits.max_concurrency, 0);

        let e = env(&[
            ("QLSTATS_RATE_LIMIT_PER_SEC", "0"),
            ("QLSTATS_MAX_CONCURRENCY", "0"),
            ("QLSTATS_REQUEST_TIMEOUT_SECS", "0"),
        ]);
        let cfg = Config::build(&Sources::default(), &e).unwrap();
        assert_eq!(cfg.http_limits, HttpLimits::disabled());

        // Нулевая ёмкость квоты — ошибка, а не «выключено».
        let err = Config::build(
            &Sources::default(),
            env(&[("QLSTATS_RATE_LIMIT_BURST", "0")]),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("QLSTATS_RATE_LIMIT_BURST"),
            "{err}"
        );
        let f = toml_file("[http]\nrate_limit_burst = 0\n");
        let err = Config::build(&sources(Some(&f)), env(&[])).unwrap_err();
        assert!(err.to_string().contains("http.rate_limit_burst"), "{err}");

        let err = Config::build(
            &Sources::default(),
            env(&[("QLSTATS_MAX_CONCURRENCY", "18446744073709551615")]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("QLSTATS_MAX_CONCURRENCY"), "{err}");

        let err = Config::build(
            &Sources::default(),
            env(&[("QLSTATS_TRUST_PROXY", "maybe")]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("QLSTATS_TRUST_PROXY"), "{err}");
    }

    #[test]
    fn example_config_parses() {
        let text = include_str!("../config.example.toml");
        let f = FileConfig::parse(text, Path::new("config.example.toml")).unwrap();
        let cfg = PartialConfig::from_file(f).resolve().unwrap();
        assert_eq!(cfg.http_limits, HttpLimits::default());
    }

    #[test]
    fn empty_env_does_not_override_toml() {
        let f = toml_file(FULL);
        let cfg = Config::build(&sources(Some(&f)), env(&[("QLSTATS_TZ", " ")])).unwrap();
        assert_eq!(cfg.tz, chrono_tz::UTC);
    }

    #[test]
    fn partial_sections() {
        let f =
            toml_file("[http]\nlisten = \"127.0.0.1:9000\"\n\n[general]\nlive_offline_secs = 5\n");
        let cfg = Config::build(&sources(Some(&f)), env(&[])).unwrap();
        assert!(cfg.zmq.is_none());
        assert_eq!(cfg.http_addr, "127.0.0.1:9000".parse().unwrap());
        assert_eq!(cfg.live_offline_secs, 5);
        assert_eq!(cfg.tz, chrono_tz::Europe::Moscow);
        assert_eq!(cfg.db_path, PathBuf::from(DEFAULT_DB_PATH));

        // Пустой файл — допустим.
        let f = toml_file("");
        assert!(Config::build(&sources(Some(&f)), env(&[])).is_ok());
    }

    #[test]
    fn zmq_split_between_toml_and_env() {
        let f = toml_file("[zmq]\nhost = \"10.0.0.1\"\n");
        let err = Config::build(&sources(Some(&f)), env(&[])).unwrap_err();
        assert_eq!(err.to_string(), "не задан zmq.port (QL_ZMQ_PORT)");

        let e = env(&[("QL_ZMQ_PORT", "28000")]);
        let err = Config::build(&sources(Some(&f)), &e).unwrap_err();
        assert_eq!(err.to_string(), "не задан zmq.password (QL_ZMQ_PASSWORD)");

        let e = env(&[("QL_ZMQ_PORT", "28000"), ("QL_ZMQ_PASSWORD", "p")]);
        let cfg = Config::build(&sources(Some(&f)), &e).unwrap();
        let z = cfg.require_zmq().unwrap();
        assert_eq!(z.endpoint(), "tcp://10.0.0.1:28000");
        assert_eq!(z.user, "stats");
    }

    #[test]
    fn require_zmq_names_key_and_env() {
        let cfg = Config::build(&Sources::default(), env(&[])).unwrap();
        let err = cfg.require_zmq().unwrap_err();
        assert_eq!(err.to_string(), "не задан zmq.host (QL_ZMQ_HOST)");
    }

    #[test]
    fn invalid_values_name_their_source() {
        let f = toml_file("[http]\nlisten = \"nonsense\"\n");
        let err = Config::build(&sources(Some(&f)), env(&[])).unwrap_err();
        assert!(err.to_string().contains("http.listen"), "{err}");

        // Некорректное значение в TOML не мешает, если его перекрывает env.
        let cfg = Config::build(
            &sources(Some(&f)),
            env(&[("QLSTATS_HTTP_ADDR", "127.0.0.1:1")]),
        )
        .unwrap();
        assert_eq!(cfg.http_addr, "127.0.0.1:1".parse().unwrap());

        let err =
            Config::build(&Sources::default(), env(&[("QLSTATS_TZ", "Mars/Olympus")])).unwrap_err();
        assert!(err.to_string().contains("QLSTATS_TZ"), "{err}");

        let f = toml_file("[general]\ntz = \"Mars/Olympus\"\n");
        let err = Config::build(&sources(Some(&f)), env(&[])).unwrap_err();
        assert!(err.to_string().contains("general.tz"), "{err}");
    }

    #[test]
    fn parse_error_hides_password() {
        // Синтаксическая ошибка в строке с паролем.
        let text = "[zmq]\nhost = \"h\"\npassword = \"hunter2 \n";
        let err = FileConfig::parse(text, Path::new("/etc/q.toml")).unwrap_err();
        let s = err.to_string();
        assert!(!s.contains("hunter2"), "{s}");
        assert!(s.contains("/etc/q.toml"), "{s}");
        assert!(s.contains("строка 3"), "{s}");

        // Ошибка типа: пароль в поле port, число в поле password.
        for text in [
            "[zmq]\nport = \"hunter2\"\n",
            "[zmq]\npassword = 123456789\n",
            "[zmq]\npassword = [\"hunter2\"]\n",
        ] {
            let err = FileConfig::parse(text, Path::new("q.toml")).unwrap_err();
            let s = err.to_string();
            assert!(!s.contains("hunter2") && !s.contains("123456789"), "{s}");
            assert!(s.contains("строка 2"), "{s}");
        }
    }

    #[test]
    fn unknown_key_is_error() {
        let err =
            FileConfig::parse("[zmq]\npasword = \"hunter2\"\n", Path::new("q.toml")).unwrap_err();
        assert!(err.to_string().contains("строка 2"), "{err}");
        assert!(err.to_string().contains("pasword"), "{err}");
        assert!(!err.to_string().contains("hunter2"), "{err}");
    }

    #[test]
    fn missing_config_file_is_error() {
        let src = Sources {
            config_file: Some("/nonexistent/qlstats.toml".into()),
            ..Sources::default()
        };
        let err = Config::build(&src, env(&[])).unwrap_err();
        assert!(
            err.to_string().contains("/nonexistent/qlstats.toml"),
            "{err}"
        );
    }

    #[test]
    fn redact_quoted_works() {
        assert_eq!(
            redact_quoted("invalid type: string \"abc\", expected u16"),
            "invalid type: string \"…\", expected u16"
        );
        assert_eq!(redact_quoted("integer `5`"), "integer `…`");
        assert_eq!(redact_quoted("open \"abc"), "open \"…");
    }
}
