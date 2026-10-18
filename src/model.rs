//! Типы событий ZMQ-потока Quake Live и терпимый разбор.
//!
//! Правила терпимости: булевы — `true/false` и `0/1` (и строки), числа — целые,
//! дробные или строкой, `STEAM_ID` — строкой или числом (хранится строкой),
//! неизвестные поля игнорируются, отсутствующие дают `None`, неизвестный `TYPE`
//! даёт [`Event::Other`]. Поле неподходящего типа тоже даёт `None`, а не ошибку.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer};
use serde_json::Value;
use thiserror::Error;

pub const INVALID_TYPE: &str = "_INVALID";

// ---------------------------------------------------------------- deserializers

fn value_to_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_f64().map(|f| f != 0.0),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn value_to_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse::<f64>().ok().map(|f| f as i64),
        Value::Bool(b) => Some(*b as i64),
        _ => None,
    }
}

fn value_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

pub fn de_bool<'de, D: Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    Ok(Option::<Value>::deserialize(d)?
        .as_ref()
        .and_then(value_to_bool))
}

pub fn de_i64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    Ok(Option::<Value>::deserialize(d)?
        .as_ref()
        .and_then(value_to_i64))
}

pub fn de_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(Option::<Value>::deserialize(d)?
        .as_ref()
        .and_then(value_to_string))
}

fn de_steam<'de, D: Deserializer<'de>>(d: D) -> Result<Option<SteamId>, D::Error> {
    Ok(Option::<Value>::deserialize(d)?
        .as_ref()
        .and_then(|v| match v {
            Value::Number(n) => n
                .as_u64()
                .map(|u| u.to_string())
                .or_else(|| n.as_f64().map(|f| (f as u64).to_string())),
            Value::String(s) => Some(s.trim().to_string()),
            _ => None,
        })
        .filter(|s| !s.is_empty())
        .map(SteamId))
}

fn de_team<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Team>, D::Error> {
    Ok(Option::<Value>::deserialize(d)?
        .as_ref()
        .and_then(Team::from_value))
}

/// Объект или `None`, если там что-то другое (например, `null`).
fn de_obj<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let v = Option::<Value>::deserialize(d)?;
    Ok(v.and_then(|v| serde_json::from_value(v).ok()))
}

fn de_vec<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let v = Option::<Value>::deserialize(d)?;
    Ok(match v {
        Some(Value::Array(items)) => items
            .into_iter()
            .filter_map(|i| serde_json::from_value(i).ok())
            .collect(),
        _ => Vec::new(),
    })
}

fn de_weapons<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<BTreeMap<String, WeaponStats>>, D::Error> {
    let v = Option::<Value>::deserialize(d)?;
    Ok(match v {
        Some(Value::Object(m)) => Some(
            m.into_iter()
                .filter_map(|(k, v)| serde_json::from_value(v).ok().map(|w| (k, w)))
                .collect(),
        ),
        _ => None,
    })
}

// ---------------------------------------------------------------- basic types

/// `STEAM_ID`, всегда строкой. У ботов — `"0"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SteamId(pub String);

impl SteamId {
    pub fn is_bot(&self) -> bool {
        self.0 == "0"
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Team {
    Free,
    Red,
    Blue,
    Spectator,
}

impl Team {
    /// Число (0 free, 1 red, 2 blue, 3 spectator) или строка (`FREE`, `RED`, ...).
    pub fn from_value(v: &Value) -> Option<Team> {
        match v {
            Value::Number(n) => match n.as_i64()? {
                0 => Some(Team::Free),
                1 => Some(Team::Red),
                2 => Some(Team::Blue),
                3 => Some(Team::Spectator),
                _ => None,
            },
            Value::String(s) => match s.trim().to_ascii_uppercase().as_str() {
                "FREE" | "0" => Some(Team::Free),
                "RED" | "1" => Some(Team::Red),
                "BLUE" | "2" => Some(Team::Blue),
                "SPECTATOR" | "3" => Some(Team::Spectator),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Team::Free => "free",
            Team::Red => "red",
            Team::Blue => "blue",
            Team::Spectator => "spectator",
        }
    }
}

/// Блок `KILLER`/`VICTIM` (и игрок в `PLAYER_SWITCHTEAM`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PlayerRef {
    #[serde(rename = "NAME", deserialize_with = "de_string")]
    pub name: Option<String>,
    #[serde(rename = "STEAM_ID", deserialize_with = "de_steam")]
    pub steam_id: Option<SteamId>,
    #[serde(rename = "TEAM", deserialize_with = "de_team")]
    pub team: Option<Team>,
    #[serde(rename = "OLD_TEAM", deserialize_with = "de_team")]
    pub old_team: Option<Team>,
    #[serde(rename = "BOT", deserialize_with = "de_bool")]
    pub bot: Option<bool>,
}

impl PlayerRef {
    /// В блоках бот определяется по `BOT = true`; `STEAM_ID == "0"` — запасной признак.
    pub fn is_bot(&self) -> bool {
        self.bot == Some(true) || self.steam_id.as_ref().is_some_and(SteamId::is_bot)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StartedPlayer {
    #[serde(rename = "NAME", deserialize_with = "de_string")]
    pub name: Option<String>,
    #[serde(rename = "STEAM_ID", deserialize_with = "de_steam")]
    pub steam_id: Option<SteamId>,
    #[serde(rename = "TEAM", deserialize_with = "de_team")]
    pub team: Option<Team>,
}

/// Лимиты матча (общие для `MATCH_STARTED` и `MATCH_REPORT`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Limits {
    #[serde(rename = "FRAG_LIMIT", deserialize_with = "de_i64")]
    pub frag: Option<i64>,
    #[serde(rename = "TIME_LIMIT", deserialize_with = "de_i64")]
    pub time_min: Option<i64>,
    #[serde(rename = "CAPTURE_LIMIT", deserialize_with = "de_i64")]
    pub capture: Option<i64>,
    #[serde(rename = "ROUND_LIMIT", deserialize_with = "de_i64")]
    pub round: Option<i64>,
    #[serde(rename = "SCORE_LIMIT", deserialize_with = "de_i64")]
    pub score: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MatchStarted {
    #[serde(rename = "MATCH_GUID", deserialize_with = "de_string")]
    pub match_guid: Option<String>,
    #[serde(rename = "GAME_TYPE", deserialize_with = "de_string")]
    pub game_type: Option<String>,
    #[serde(rename = "MAP", deserialize_with = "de_string")]
    pub map: Option<String>,
    #[serde(rename = "FACTORY_TITLE", deserialize_with = "de_string")]
    pub factory_title: Option<String>,
    #[serde(rename = "SERVER_TITLE", deserialize_with = "de_string")]
    pub server_title: Option<String>,
    #[serde(flatten)]
    pub limits: Limits,
    #[serde(rename = "PLAYERS", deserialize_with = "de_vec")]
    pub players: Vec<StartedPlayer>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MatchReport {
    #[serde(rename = "MATCH_GUID", deserialize_with = "de_string")]
    pub match_guid: Option<String>,
    #[serde(rename = "ABORTED", deserialize_with = "de_bool")]
    pub aborted: Option<bool>,
    #[serde(rename = "GAME_TYPE", deserialize_with = "de_string")]
    pub game_type: Option<String>,
    #[serde(rename = "MAP", deserialize_with = "de_string")]
    pub map: Option<String>,
    #[serde(rename = "SERVER_TITLE", deserialize_with = "de_string")]
    pub server_title: Option<String>,
    #[serde(rename = "EXIT_MSG", deserialize_with = "de_string")]
    pub exit_msg: Option<String>,
    #[serde(rename = "GAME_LENGTH", deserialize_with = "de_i64")]
    pub game_length: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct WeaponStats {
    #[serde(rename = "S", deserialize_with = "de_i64")]
    pub shots: Option<i64>,
    #[serde(rename = "H", deserialize_with = "de_i64")]
    pub hits: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PlayerStats {
    #[serde(rename = "MATCH_GUID", deserialize_with = "de_string")]
    pub match_guid: Option<String>,
    #[serde(rename = "NAME", deserialize_with = "de_string")]
    pub name: Option<String>,
    #[serde(rename = "STEAM_ID", deserialize_with = "de_steam")]
    pub steam_id: Option<SteamId>,
    #[serde(rename = "WARMUP", deserialize_with = "de_bool")]
    pub warmup: Option<bool>,
    #[serde(rename = "ABORTED", deserialize_with = "de_bool")]
    pub aborted: Option<bool>,
    #[serde(rename = "QUIT", deserialize_with = "de_bool")]
    pub quit: Option<bool>,
    #[serde(rename = "RANK", deserialize_with = "de_i64")]
    pub rank: Option<i64>,
    #[serde(rename = "SCORE", deserialize_with = "de_i64")]
    pub score: Option<i64>,
    #[serde(rename = "WIN", deserialize_with = "de_i64")]
    pub win: Option<i64>,
    #[serde(rename = "TEAM", deserialize_with = "de_team")]
    pub team: Option<Team>,
    #[serde(rename = "MAX_STREAK", deserialize_with = "de_i64")]
    pub max_streak: Option<i64>,
    #[serde(rename = "WEAPONS", deserialize_with = "de_weapons")]
    pub weapons: Option<BTreeMap<String, WeaponStats>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PlayerDeath {
    #[serde(rename = "MATCH_GUID", deserialize_with = "de_string")]
    pub match_guid: Option<String>,
    #[serde(rename = "TIME", deserialize_with = "de_i64")]
    pub time: Option<i64>,
    #[serde(rename = "MOD", deserialize_with = "de_string")]
    pub mod_: Option<String>,
    #[serde(rename = "SUICIDE", deserialize_with = "de_bool")]
    pub suicide: Option<bool>,
    #[serde(rename = "TEAMKILL", deserialize_with = "de_bool")]
    pub teamkill: Option<bool>,
    #[serde(rename = "WARMUP", deserialize_with = "de_bool")]
    pub warmup: Option<bool>,
    #[serde(rename = "KILLER", deserialize_with = "de_obj")]
    pub killer: Option<PlayerRef>,
    #[serde(rename = "VICTIM", deserialize_with = "de_obj")]
    pub victim: Option<PlayerRef>,
}

/// `PLAYER_CONNECT`, `PLAYER_DISCONNECT`, `PLAYER_MEDAL` — общий набор полей.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PlayerEvent {
    #[serde(rename = "MATCH_GUID", deserialize_with = "de_string")]
    pub match_guid: Option<String>,
    #[serde(rename = "NAME", deserialize_with = "de_string")]
    pub name: Option<String>,
    #[serde(rename = "STEAM_ID", deserialize_with = "de_steam")]
    pub steam_id: Option<SteamId>,
    #[serde(rename = "TIME", deserialize_with = "de_i64")]
    pub time: Option<i64>,
    #[serde(rename = "WARMUP", deserialize_with = "de_bool")]
    pub warmup: Option<bool>,
    #[serde(rename = "MEDAL", deserialize_with = "de_string")]
    pub medal: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PlayerSwitchTeam {
    #[serde(rename = "MATCH_GUID", deserialize_with = "de_string")]
    pub match_guid: Option<String>,
    #[serde(rename = "KILLER", deserialize_with = "de_obj")]
    pub player: Option<PlayerRef>,
    #[serde(rename = "TIME", deserialize_with = "de_i64")]
    pub time: Option<i64>,
    #[serde(rename = "WARMUP", deserialize_with = "de_bool")]
    pub warmup: Option<bool>,
}

#[derive(Debug, Clone)]
pub enum Event {
    MatchStarted(MatchStarted),
    MatchReport(MatchReport),
    PlayerStats(PlayerStats),
    PlayerDeath(PlayerDeath),
    /// Дубль `PLAYER_DEATH`, в расчётах не используется.
    PlayerKill(PlayerDeath),
    PlayerConnect(PlayerEvent),
    PlayerDisconnect(PlayerEvent),
    PlayerSwitchTeam(PlayerSwitchTeam),
    PlayerMedal(PlayerEvent),
    /// `ROUND_OVER` и любые неизвестные типы.
    Other {
        kind: String,
        match_guid: Option<String>,
        warmup: Option<bool>,
    },
}

impl Event {
    pub fn match_guid(&self) -> Option<&str> {
        match self {
            Event::MatchStarted(e) => e.match_guid.as_deref(),
            Event::MatchReport(e) => e.match_guid.as_deref(),
            Event::PlayerStats(e) => e.match_guid.as_deref(),
            Event::PlayerDeath(e) | Event::PlayerKill(e) => e.match_guid.as_deref(),
            Event::PlayerConnect(e) | Event::PlayerDisconnect(e) | Event::PlayerMedal(e) => {
                e.match_guid.as_deref()
            }
            Event::PlayerSwitchTeam(e) => e.match_guid.as_deref(),
            Event::Other { match_guid, .. } => match_guid.as_deref(),
        }
    }

    /// Значение `WARMUP`, если поле есть в событии.
    pub fn warmup(&self) -> Option<bool> {
        match self {
            Event::MatchStarted(_) | Event::MatchReport(_) => None,
            Event::PlayerStats(e) => e.warmup,
            Event::PlayerDeath(e) | Event::PlayerKill(e) => e.warmup,
            Event::PlayerConnect(e) | Event::PlayerDisconnect(e) | Event::PlayerMedal(e) => {
                e.warmup
            }
            Event::PlayerSwitchTeam(e) => e.warmup,
            Event::Other { warmup, .. } => *warmup,
        }
    }

    /// Пары `(STEAM_ID, NAME)` людей из события — для таблицы `players`.
    pub fn human_names(&self) -> Vec<(SteamId, String)> {
        fn push(out: &mut Vec<(SteamId, String)>, sid: &Option<SteamId>, name: &Option<String>) {
            if let (Some(s), Some(n)) = (sid, name)
                && !s.is_bot()
                && !n.is_empty()
            {
                out.push((s.clone(), n.clone()));
            }
        }
        let mut out = Vec::new();
        match self {
            Event::MatchStarted(e) => {
                for p in &e.players {
                    push(&mut out, &p.steam_id, &p.name);
                }
            }
            Event::PlayerStats(e) => push(&mut out, &e.steam_id, &e.name),
            Event::PlayerDeath(e) | Event::PlayerKill(e) => {
                for b in [&e.killer, &e.victim].into_iter().flatten() {
                    if !b.is_bot() {
                        push(&mut out, &b.steam_id, &b.name);
                    }
                }
            }
            Event::PlayerConnect(e) | Event::PlayerDisconnect(e) | Event::PlayerMedal(e) => {
                push(&mut out, &e.steam_id, &e.name)
            }
            Event::PlayerSwitchTeam(e) => {
                if let Some(p) = &e.player {
                    push(&mut out, &p.steam_id, &p.name)
                }
            }
            Event::MatchReport(_) | Event::Other { .. } => {}
        }
        out
    }
}

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("не JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("нет поля TYPE")]
    NoType,
}

/// Разобранный конверт `{"TYPE": ..., "DATA": {...}}`.
#[derive(Debug, Clone)]
pub struct Parsed {
    pub kind: String,
    pub event: Event,
}

/// Декодирует байты фрейма в текст (lossy UTF-8).
pub fn decode_lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Минимальный разбор для `raw_events`: тип (или `_INVALID`) и `MATCH_GUID`.
pub fn extract_meta(text: &str) -> (String, Option<String>) {
    match serde_json::from_str::<Value>(text) {
        Ok(v) => {
            let kind = v.get("TYPE").and_then(Value::as_str).map(str::to_string);
            let guid = v
                .get("DATA")
                .and_then(|d| d.get("MATCH_GUID"))
                .and_then(value_to_string)
                .filter(|s| !s.is_empty());
            match kind {
                Some(k) => (k, guid),
                None => (INVALID_TYPE.to_string(), guid),
            }
        }
        Err(_) => (INVALID_TYPE.to_string(), None),
    }
}

/// Полный разбор события. Ошибка только если это не JSON или нет `TYPE`.
pub fn parse(text: &str) -> Result<Parsed, ParseError> {
    let v: Value = serde_json::from_str(text)?;
    parse_value(v)
}

pub fn parse_value(mut v: Value) -> Result<Parsed, ParseError> {
    let kind = v
        .get("TYPE")
        .and_then(Value::as_str)
        .ok_or(ParseError::NoType)?
        .to_string();
    let data = v
        .get_mut("DATA")
        .map(Value::take)
        .unwrap_or(Value::Object(Default::default()));
    let event = parse_data(&kind, data);
    Ok(Parsed { kind, event })
}

fn from<T: serde::de::DeserializeOwned + Default>(data: Value) -> T {
    // Все поля терпимые, поэтому ошибка возможна, только если DATA — не объект.
    serde_json::from_value(data).unwrap_or_default()
}

/// Разбор `DATA` по известному `TYPE`.
pub fn parse_data(kind: &str, data: Value) -> Event {
    match kind {
        "MATCH_STARTED" => Event::MatchStarted(from(data)),
        "MATCH_REPORT" => Event::MatchReport(from(data)),
        "PLAYER_STATS" => Event::PlayerStats(from(data)),
        "PLAYER_DEATH" => Event::PlayerDeath(from(data)),
        "PLAYER_KILL" => Event::PlayerKill(from(data)),
        "PLAYER_CONNECT" => Event::PlayerConnect(from(data)),
        "PLAYER_DISCONNECT" => Event::PlayerDisconnect(from(data)),
        "PLAYER_SWITCHTEAM" => Event::PlayerSwitchTeam(from(data)),
        "PLAYER_MEDAL" => Event::PlayerMedal(from(data)),
        other => Event::Other {
            kind: other.to_string(),
            match_guid: data.get("MATCH_GUID").and_then(value_to_string),
            warmup: data.get("WARMUP").and_then(value_to_bool),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bools_accept_numbers_and_strings() {
        let e = parse(r#"{"TYPE":"PLAYER_STATS","DATA":{"WARMUP":1,"QUIT":"0","ABORTED":false}}"#)
            .unwrap();
        let Event::PlayerStats(s) = e.event else {
            panic!()
        };
        assert_eq!(s.warmup, Some(true));
        assert_eq!(s.quit, Some(false));
        assert_eq!(s.aborted, Some(false));
    }

    #[test]
    fn steam_id_string_or_number() {
        let e =
            parse(r#"{"TYPE":"PLAYER_CONNECT","DATA":{"STEAM_ID":76561198045366387}}"#).unwrap();
        let Event::PlayerConnect(c) = e.event else {
            panic!()
        };
        assert_eq!(c.steam_id.unwrap().0, "76561198045366387");
        let e = parse(r#"{"TYPE":"PLAYER_CONNECT","DATA":{"STEAM_ID":0}}"#).unwrap();
        let Event::PlayerConnect(c) = e.event else {
            panic!()
        };
        assert!(c.steam_id.unwrap().is_bot());
        let e = parse(r#"{"TYPE":"PLAYER_CONNECT","DATA":{"STEAM_ID":"0"}}"#).unwrap();
        let Event::PlayerConnect(c) = e.event else {
            panic!()
        };
        assert!(c.steam_id.unwrap().is_bot());
    }

    #[test]
    fn extra_missing_and_wrong_fields() {
        let e = parse(
            r#"{"TYPE":"PLAYER_DEATH","DATA":{"FOO":{"x":1},"MOD":"LAVA","KILLER":null,"TIME":"12","VICTIM":{"NAME":"a","STEAM_ID":"1","BOT":0,"TEAM":"FREE"},"WARMUP":"yes"}}"#,
        )
        .unwrap();
        let Event::PlayerDeath(d) = e.event else {
            panic!()
        };
        assert!(d.killer.is_none());
        assert_eq!(d.time, Some(12));
        assert_eq!(d.warmup, None);
        let v = d.victim.unwrap();
        assert!(!v.is_bot());
        assert_eq!(v.team, Some(Team::Free));
        assert!(d.match_guid.is_none());
    }

    #[test]
    fn unknown_type_and_broken_json() {
        let e = parse(r#"{"TYPE":"ROUND_OVER","DATA":{"MATCH_GUID":"g","WARMUP":0}}"#).unwrap();
        assert!(matches!(e.event, Event::Other { ref kind, .. } if kind == "ROUND_OVER"));
        assert_eq!(e.event.match_guid(), Some("g"));
        assert!(parse("{not json").is_err());
        assert!(parse(r#"{"DATA":{}}"#).is_err());
        assert_eq!(extract_meta("garbage").0, INVALID_TYPE);
        let (t, g) = extract_meta(r#"{"TYPE":"X","DATA":{"MATCH_GUID":"abc"}}"#);
        assert_eq!((t.as_str(), g.as_deref()), ("X", Some("abc")));
    }

    #[test]
    fn data_not_object_gives_defaults() {
        let e = parse(r#"{"TYPE":"PLAYER_STATS","DATA":[1,2]}"#).unwrap();
        assert!(matches!(e.event, Event::PlayerStats(_)));
    }

    #[test]
    fn invalid_utf8_lossy() {
        let s = decode_lossy(b"{\"TYPE\":\"X\xff\"}");
        assert!(s.contains('\u{FFFD}'));
    }

    #[test]
    fn real_match_started_from_sample() {
        let line = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/sample-long.jsonl"
        ))
        .unwrap();
        let started = line
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .find(|v| v["type"] == "MATCH_STARTED")
            .unwrap();
        let ev = parse_data("MATCH_STARTED", started["data"].clone());
        let Event::MatchStarted(m) = ev else { panic!() };
        assert_eq!(m.game_type.as_deref(), Some("FFA"));
        assert_eq!(m.players.len(), 2);
        assert_eq!(m.players[0].team, Some(Team::Free));
        assert!(m.players[1].steam_id.as_ref().unwrap().is_bot());
        assert_eq!(m.limits.frag, Some(50));
        assert_eq!(m.limits.time_min, Some(15));
    }
}
