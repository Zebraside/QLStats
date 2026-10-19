//! SQLite: миграции, writer, завершение матча, пересчёт и запросы агрегатов.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::mpsc;
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use chrono_tz::Tz;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use tokio::sync::watch;

use crate::model::{self, Event};
use crate::stats::{self, DeathInput, MatchInput, MatchOutcome};

/// Синтетические записи в `raw_events` о состоянии ZMQ-соединения (для `incomplete`).
pub const LINK_UP_TYPE: &str = "_CONNECTED";
pub const LINK_DOWN_TYPE: &str = "_DISCONNECTED";

const SCHEMA_V1: &str = r#"
CREATE TABLE raw_events(
  id          INTEGER PRIMARY KEY,
  received_at INTEGER NOT NULL,
  type        TEXT NOT NULL,
  match_guid  TEXT,
  payload     TEXT NOT NULL
);
CREATE INDEX raw_events_guid ON raw_events(match_guid, id);
CREATE INDEX raw_events_type ON raw_events(type);

CREATE TABLE matches(
  guid          TEXT PRIMARY KEY,
  game_type     TEXT,
  map           TEXT,
  started_at    INTEGER,
  finished_at   INTEGER NOT NULL,
  day_key       TEXT NOT NULL,
  aborted       INTEGER NOT NULL,
  exit_msg      TEXT,
  game_length   INTEGER,
  human_players INTEGER NOT NULL,
  counted       INTEGER NOT NULL,
  skip_reason   TEXT,
  incomplete    INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX matches_counted_day ON matches(counted, day_key);

CREATE TABLE match_players(
  match_guid TEXT NOT NULL,
  steam_id   TEXT NOT NULL,
  name TEXT,
  has_stats INTEGER,
  score INTEGER, rank INTEGER, win_raw INTEGER, quit INTEGER,
  max_streak_reported INTEGER,
  weapons TEXT,
  kills INTEGER, deaths INTEGER, suicides INTEGER, env_deaths INTEGER,
  best_streak INTEGER,
  is_winner INTEGER,
  PRIMARY KEY (match_guid, steam_id)
);

CREATE TABLE match_player_weapons(
  match_guid TEXT NOT NULL, steam_id TEXT NOT NULL, weapon TEXT NOT NULL,
  shots INTEGER, hits INTEGER,
  PRIMARY KEY (match_guid, steam_id, weapon)
);

CREATE TABLE deaths(
  id INTEGER PRIMARY KEY,
  raw_event_id INTEGER REFERENCES raw_events(id),
  match_guid TEXT, time INTEGER,
  victim_steam_id TEXT, killer_steam_id TEXT,
  mod TEXT,
  category TEXT CHECK (category IN ('kill','suicide','environment','ignored'))
);
CREATE INDEX deaths_guid ON deaths(match_guid);

CREATE TABLE players(steam_id TEXT PRIMARY KEY, last_name TEXT, last_seen INTEGER);
"#;

/// Засчитанные убийства по оружию (из `MOD`) на игрока в матче, JSON `{"RAILGUN":12,...}`.
/// У матчей, записанных до миграции, `NULL` до `qlstats recompute`.
const SCHEMA_V2: &str = "ALTER TABLE match_players ADD COLUMN weapon_kills TEXT;";

const MIGRATIONS: &[&str] = &[SCHEMA_V1, SCHEMA_V2];

/// Открывает (и при необходимости создаёт) БД, применяет PRAGMA и миграции.
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("не удалось создать каталог {}", dir.display()))?;
    }
    let conn = Connection::open(path)
        .with_context(|| format!("не удалось открыть БД {}", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    migrate(&conn)?;
    Ok(conn)
}

pub fn migrate(conn: &Connection) -> Result<()> {
    let ver: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(ver as usize) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
        tracing::info!(version = i + 1, "применена миграция БД");
    }
    Ok(())
}

// ---------------------------------------------------------------- raw + players

fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

pub fn insert_raw(
    conn: &Connection,
    received_at: i64,
    kind: &str,
    guid: Option<&str>,
    payload: &str,
) -> Result<i64> {
    Ok(insert_raw_sql(conn, received_at, kind, guid, payload)?)
}

fn insert_raw_sql(
    conn: &Connection,
    received_at: i64,
    kind: &str,
    guid: Option<&str>,
    payload: &str,
) -> rusqlite::Result<i64> {
    conn.prepare_cached(
        "INSERT INTO raw_events(received_at, type, match_guid, payload) VALUES (?1, ?2, ?3, ?4)",
    )?
    .execute(params![received_at, kind, guid, payload])?;
    Ok(conn.last_insert_rowid())
}

pub fn upsert_players(conn: &Connection, ev: &Event, received_at: i64) -> Result<()> {
    Ok(upsert_players_sql(conn, ev, received_at)?)
}

fn upsert_players_sql(conn: &Connection, ev: &Event, received_at: i64) -> rusqlite::Result<()> {
    let mut st = conn.prepare_cached(
        "INSERT INTO players(steam_id, last_name, last_seen) VALUES (?1, ?2, ?3)
         ON CONFLICT(steam_id) DO UPDATE SET last_name = excluded.last_name, last_seen = excluded.last_seen
         WHERE excluded.last_seen >= players.last_seen",
    )?;
    for (sid, name) in ev.human_names() {
        st.execute(params![sid.0, name, received_at])?;
    }
    Ok(())
}

// ---------------------------------------------------------------- finalize

/// Завершает матч по `MATCH_REPORT` (вызывается внутри транзакции).
/// Возвращает `None`, если матч с этим GUID уже записан (повторный REPORT).
pub fn finalize_match(
    conn: &Connection,
    guid: &str,
    report_raw_id: i64,
    tz: Tz,
) -> Result<Option<MatchOutcome>> {
    let exists: Option<i64> = conn
        .query_row("SELECT 1 FROM matches WHERE guid = ?1", [guid], |r| {
            r.get(0)
        })
        .optional()?;
    if exists.is_some() {
        tracing::info!(%guid, "повторный MATCH_REPORT проигнорирован");
        return Ok(None);
    }

    let mut st = conn.prepare_cached(
        "SELECT id, received_at, type, payload FROM raw_events
         WHERE match_guid = ?1 AND id <= ?2 ORDER BY id",
    )?;
    let rows = st
        .query_map(params![guid, report_raw_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut started = None;
    let mut report = None;
    let mut finished_at = 0;
    let mut stats_v = Vec::new();
    let mut deaths = Vec::new();
    let first_id = rows.first().map(|r| r.0).unwrap_or(report_raw_id);
    for (id, received_at, _kind, payload) in rows {
        let Ok(parsed) = model::parse(&payload) else {
            continue;
        };
        match parsed.event {
            Event::MatchStarted(s) if id < report_raw_id => started = Some((received_at, s)),
            Event::MatchReport(r) if id == report_raw_id => {
                report = Some(r);
                finished_at = received_at;
            }
            Event::PlayerStats(s) if s.warmup != Some(true) => stats_v.push(s),
            Event::PlayerDeath(d) if d.warmup != Some(true) => {
                deaths.push(DeathInput { raw_id: id, ev: d })
            }
            _ => {}
        }
    }
    let Some(report) = report else {
        anyhow::bail!("MATCH_REPORT id={report_raw_id} для {guid} не найден или не разобран");
    };

    let gap: Option<i64> = conn
        .query_row(
            // `_CONNECTED` внутри матча тоже значит разрыв (например, рестарт после падения).
            "SELECT 1 FROM raw_events WHERE type IN (?1, ?2) AND id > ?3 AND id < ?4 LIMIT 1",
            params![LINK_DOWN_TYPE, LINK_UP_TYPE, first_id, report_raw_id],
            |r| r.get(0),
        )
        .optional()?;

    let input = MatchInput {
        guid: guid.to_string(),
        started,
        report,
        finished_at,
        stats: stats_v,
        deaths,
        incomplete: gap.is_some(),
    };
    let out = stats::compute_match(&input);
    write_outcome(conn, &out, tz)?;
    Ok(Some(out))
}

fn write_outcome(conn: &Connection, out: &MatchOutcome, tz: Tz) -> Result<()> {
    let m = &out.row;
    conn.prepare_cached(
        "INSERT INTO matches(guid, game_type, map, started_at, finished_at, day_key, aborted,
            exit_msg, game_length, human_players, counted, skip_reason, incomplete)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
    )?
    .execute(params![
        m.guid,
        m.game_type,
        m.map,
        m.started_at,
        m.finished_at,
        stats::day_key(m.finished_at, tz),
        m.aborted,
        m.exit_msg,
        m.game_length,
        m.human_players,
        m.counted,
        m.skip_reason.map(|s| s.as_str()),
        m.incomplete,
    ])?;
    let mut ins_p = conn.prepare_cached(
        "INSERT INTO match_players(match_guid, steam_id, name, has_stats, score, rank, win_raw, quit,
            max_streak_reported, weapons, weapon_kills, kills, deaths, suicides, env_deaths,
            best_streak, is_winner)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
    )?;
    for p in &out.players {
        ins_p.execute(params![
            m.guid,
            p.steam_id,
            p.name,
            p.has_stats,
            p.score,
            p.rank,
            p.win_raw,
            p.quit,
            p.max_streak_reported,
            p.weapons_json,
            p.weapon_kills_json,
            p.kills,
            p.deaths,
            p.suicides,
            p.env_deaths,
            p.best_streak,
            p.is_winner,
        ])?;
    }
    let mut ins_w = conn.prepare_cached(
        "INSERT OR REPLACE INTO match_player_weapons(match_guid, steam_id, weapon, shots, hits)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for w in &out.weapons {
        ins_w.execute(params![m.guid, w.steam_id, w.weapon, w.shots, w.hits])?;
    }
    let mut ins_d = conn.prepare_cached(
        "INSERT INTO deaths(raw_event_id, match_guid, time, victim_steam_id, killer_steam_id, mod, category)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for d in &out.deaths {
        ins_d.execute(params![
            d.raw_id,
            m.guid,
            d.time,
            d.victim,
            d.killer,
            d.mod_,
            d.category.as_str()
        ])?;
    }
    Ok(())
}

fn log_outcome(out: &MatchOutcome) {
    let m = &out.row;
    if m.counted {
        tracing::info!(
            guid = %m.guid,
            game_type = m.game_type.as_deref().unwrap_or(""),
            map = m.map.as_deref().unwrap_or(""),
            humans = m.human_players,
            incomplete = m.incomplete,
            "матч засчитан"
        );
    } else {
        tracing::info!(
            guid = %m.guid,
            game_type = m.game_type.as_deref().unwrap_or(""),
            humans = m.human_players,
            skip_reason = m.skip_reason.map(|s| s.as_str()).unwrap_or(""),
            "матч не засчитан"
        );
    }
}

// ---------------------------------------------------------------- writer

/// Сообщение в writer-поток.
#[derive(Debug)]
pub enum WriterMsg {
    /// Фрейм из ZMQ (или из replay) с временем получения, UTC мс.
    Frame { received_at: i64, bytes: Vec<u8> },
    /// Изменение состояния соединения (пишется в `raw_events` как `_CONNECTED`/`_DISCONNECTED`).
    Link { connected: bool, at: i64 },
    /// Ответить, когда все предыдущие сообщения обработаны.
    Flush(mpsc::Sender<()>),
}

/// Единственное write-соединение и логика обработки входящих событий.
pub struct Writer {
    conn: Connection,
    tz: Tz,
    notify: Option<watch::Sender<u64>>,
}

impl Writer {
    pub fn new(conn: Connection, tz: Tz, notify: Option<watch::Sender<u64>>) -> Self {
        Self { conn, tz, notify }
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Обрабатывает одно сообщение. Ошибки пишутся в лог и не прерывают поток.
    pub fn handle(&mut self, msg: WriterMsg) {
        match msg {
            WriterMsg::Frame { received_at, bytes } => {
                if let Err(e) = self.process_frame(received_at, &bytes) {
                    tracing::error!(error = %format!("{e:#}"), "ошибка обработки события");
                }
            }
            WriterMsg::Link { connected, at } => {
                let kind = if connected {
                    LINK_UP_TYPE
                } else {
                    LINK_DOWN_TYPE
                };
                let payload = format!(r#"{{"TYPE":"{kind}","DATA":{{}}}}"#);
                if let Err(e) = insert_raw(&self.conn, at, kind, None, &payload) {
                    tracing::error!(error = %e, "ошибка записи события соединения");
                }
            }
            WriterMsg::Flush(tx) => {
                let _ = tx.send(());
            }
        }
    }

    /// Шаги 2–4 из ARCHITECTURE §6. Возвращает результат завершения матча, если был REPORT.
    pub fn process_frame(
        &mut self,
        received_at: i64,
        bytes: &[u8],
    ) -> Result<Option<MatchOutcome>> {
        let text = model::decode_lossy(bytes);
        let (kind, guid) = model::extract_meta(&text);
        // Сервер шлёт JSON с отступами (в 1,8 раза длиннее компактного). В raw_events
        // кладём компактную форму с тем же порядком ключей; не-JSON сохраняется как есть.
        let (text, parsed) = match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(v) => (
                serde_json::to_string(&v).unwrap_or(text),
                model::parse_value(v),
            ),
            Err(e) => (text, Err(e.into())),
        };
        if let Err(e) = &parsed {
            tracing::warn!(error = %e, bytes = bytes.len(), "_INVALID событие");
        }
        // Сырое событие нельзя потерять: при занятой БД (например, идёт recompute) ждём.
        let mut attempt = 0u32;
        let raw_id = loop {
            let res = (|| -> rusqlite::Result<i64> {
                let tx = self.conn.transaction()?;
                let id = insert_raw_sql(&tx, received_at, &kind, guid.as_deref(), &text)?;
                if let Ok(p) = &parsed {
                    upsert_players_sql(&tx, &p.event, received_at)?;
                }
                tx.commit()?;
                Ok(id)
            })();
            match res {
                Ok(id) => break id,
                Err(e) if is_busy(&e) && attempt < 600 => {
                    attempt += 1;
                    if attempt == 1 {
                        tracing::warn!("БД занята, запись сырого события повторяется");
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                Err(e) => return Err(e.into()),
            }
        };
        if kind != "MATCH_REPORT" {
            return Ok(None);
        }
        let Some(guid) = guid else {
            tracing::warn!(raw_id, "MATCH_REPORT без MATCH_GUID");
            return Ok(None);
        };
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = match finalize_match(&tx, &guid, raw_id, self.tz) {
            Ok(o) => o,
            Err(e) => {
                drop(tx); // ROLLBACK
                tracing::error!(%guid, error = %format!("{e:#}"), "ошибка завершения матча");
                return Err(e);
            }
        };
        tx.commit()?;
        if let Some(o) = &out {
            log_outcome(o);
            if let Some(n) = &self.notify {
                n.send_modify(|v| *v += 1);
            }
        }
        Ok(out)
    }
}

/// Запускает writer-поток. Он завершается, когда закрыт канал.
pub fn spawn_writer(mut writer: Writer, rx: mpsc::Receiver<WriterMsg>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("store-writer".into())
        .spawn(move || {
            while let Ok(msg) = rx.recv() {
                writer.handle(msg);
            }
            tracing::debug!("writer остановлен");
        })
        .expect("не удалось запустить writer-поток")
}

// ---------------------------------------------------------------- recompute

#[derive(Debug, Default, Serialize)]
pub struct RecomputeSummary {
    pub raw_events: u64,
    pub matches: u64,
    pub counted: u64,
}

/// Резервная копия через `VACUUM INTO`.
pub fn backup(conn: &Connection, dest: &Path) -> Result<()> {
    conn.execute("VACUUM INTO ?1", [dest.to_string_lossy()])?;
    Ok(())
}

/// Пересоздаёт все производные таблицы из `raw_events` в одной транзакции.
pub fn recompute(conn: &mut Connection, tz: Tz) -> Result<RecomputeSummary> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(
        "DELETE FROM deaths; DELETE FROM match_player_weapons; DELETE FROM match_players;
         DELETE FROM matches; DELETE FROM players;",
    )?;
    let mut sum = RecomputeSummary::default();
    let mut last_id = 0i64;
    loop {
        let batch: Vec<(i64, i64, String, Option<String>, String)> = {
            let mut st = tx.prepare_cached(
                "SELECT id, received_at, type, match_guid, payload FROM raw_events
                 WHERE id > ?1 ORDER BY id LIMIT 5000",
            )?;
            st.query_map([last_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        if batch.is_empty() {
            break;
        }
        for (id, received_at, kind, guid, payload) in batch {
            last_id = id;
            sum.raw_events += 1;
            if let Ok(p) = model::parse(&payload) {
                upsert_players(&tx, &p.event, received_at)?;
            }
            if kind == "MATCH_REPORT"
                && let Some(g) = guid
                && let Some(out) = finalize_match(&tx, &g, id, tz)?
            {
                sum.matches += 1;
                if out.row.counted {
                    sum.counted += 1;
                }
            }
        }
    }
    tx.commit()?;
    Ok(sum)
}

// ---------------------------------------------------------------- read queries

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PlayerTotals {
    pub steam_id: String,
    pub name: String,
    pub matches: i64,
    pub kills: i64,
    pub deaths: i64,
    pub best_streak: i64,
    pub wins: i64,
    pub accuracy: Option<f64>,
    pub suicides: i64,
    pub env_deaths: i64,
    pub fav_weapon: Option<FavWeapon>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FavWeapon {
    pub weapon: &'static str,
    pub kills: i64,
}

/// Любимое оружие по засчитанным матчам: сумма `match_players.weapon_kills` по игроку.
fn favorite_weapons(conn: &Connection) -> Result<HashMap<String, FavWeapon>> {
    let mut st = conn.prepare_cached(
        "SELECT mp.steam_id, mp.weapon_kills FROM match_players mp
         JOIN matches m ON m.guid = mp.match_guid AND m.counted = 1
         WHERE mp.weapon_kills IS NOT NULL",
    )?;
    let mut per: HashMap<String, BTreeMap<String, i64>> = HashMap::new();
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (sid, json) = row?;
        let Ok(m) = serde_json::from_str::<BTreeMap<String, i64>>(&json) else {
            tracing::warn!(steam_id = %sid, "битый match_players.weapon_kills пропущен");
            continue;
        };
        let acc = per.entry(sid).or_default();
        for (w, n) in m {
            *acc.entry(w).or_default() += n;
        }
    }
    Ok(per
        .into_iter()
        .filter_map(|(sid, m)| {
            stats::favorite_weapon(m.iter().map(|(w, n)| (w.as_str(), *n)))
                .map(|(weapon, kills)| (sid, FavWeapon { weapon, kills }))
        })
        .collect())
}

/// Таблица участников: только игроки хотя бы с одним засчитанным матчем.
pub fn player_totals(conn: &Connection) -> Result<Vec<PlayerTotals>> {
    let mut acc_st = conn.prepare_cached(
        "SELECT steam_id, AVG(acc) FROM (
            SELECT w.steam_id, w.weapon, CAST(SUM(w.hits) AS REAL) / SUM(w.shots) AS acc
            FROM match_player_weapons w JOIN matches m ON m.guid = w.match_guid AND m.counted = 1
            GROUP BY w.steam_id, w.weapon HAVING SUM(w.shots) > 0
         ) GROUP BY steam_id",
    )?;
    let mut favs = favorite_weapons(conn)?;
    let accs: HashMap<String, f64> = acc_st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut st = conn.prepare_cached(
        "SELECT mp.steam_id, COALESCE(p.last_name, MAX(mp.name), mp.steam_id),
                COUNT(*), SUM(mp.kills), SUM(mp.deaths), MAX(mp.best_streak), SUM(mp.is_winner),
                SUM(mp.suicides), SUM(mp.env_deaths)
         FROM match_players mp
         JOIN matches m ON m.guid = mp.match_guid AND m.counted = 1
         LEFT JOIN players p ON p.steam_id = mp.steam_id
         GROUP BY mp.steam_id
         ORDER BY mp.steam_id",
    )?;
    let rows = st
        .query_map([], |r| {
            let sid: String = r.get(0)?;
            Ok(PlayerTotals {
                accuracy: accs.get(&sid).map(|a| (a * 10000.0).round() / 10000.0),
                fav_weapon: favs.remove(&sid),
                steam_id: sid,
                name: r.get(1)?,
                matches: r.get(2)?,
                kills: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                deaths: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                best_streak: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                wins: r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                suicides: r.get::<_, Option<i64>>(7)?.unwrap_or(0),
                env_deaths: r.get::<_, Option<i64>>(8)?.unwrap_or(0),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PlayerRefOut {
    pub steam_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Top {
    pub value: i64,
    pub players: Vec<PlayerRefOut>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PeriodSummary {
    pub matches: i64,
    pub top_wins: Top,
    pub top_kills: Top,
}

fn top(conn: &Connection, column: &str, day: Option<&str>) -> Result<Top> {
    // column — только из фиксированного списка ниже.
    let sql = format!(
        "SELECT mp.steam_id, COALESCE(p.last_name, MAX(mp.name), mp.steam_id), SUM(mp.{column}) AS v
         FROM match_players mp
         JOIN matches m ON m.guid = mp.match_guid AND m.counted = 1
         LEFT JOIN players p ON p.steam_id = mp.steam_id
         WHERE (?1 IS NULL OR m.day_key = ?1)
         GROUP BY mp.steam_id
         ORDER BY v DESC, mp.steam_id"
    );
    let mut st = conn.prepare_cached(&sql)?;
    let rows: Vec<(String, String, i64)> = st
        .query_map([day], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let value = rows.first().map(|r| r.2).unwrap_or(0);
    let players = if value > 0 {
        rows.into_iter()
            .take_while(|r| r.2 == value)
            .map(|(steam_id, name, _)| PlayerRefOut { steam_id, name })
            .collect()
    } else {
        Vec::new()
    };
    Ok(Top { value, players })
}

/// Сводка за день (`Some(day_key)`) или за всё время (`None`).
pub fn period_summary(conn: &Connection, day: Option<&str>) -> Result<PeriodSummary> {
    let matches: i64 = conn.query_row(
        "SELECT COUNT(*) FROM matches WHERE counted = 1 AND (?1 IS NULL OR day_key = ?1)",
        [day],
        |r| r.get(0),
    )?;
    Ok(PeriodSummary {
        matches,
        top_wins: top(conn, "is_winner", day)?,
        top_kills: top(conn, "kills", day)?,
    })
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MatchListRow {
    pub guid: String,
    pub game_type: Option<String>,
    pub map: Option<String>,
    pub started_at: Option<i64>,
    pub finished_at: i64,
    pub day_key: String,
    pub aborted: bool,
    pub exit_msg: Option<String>,
    pub game_length: Option<i64>,
    pub human_players: i64,
    pub counted: bool,
    pub skip_reason: Option<String>,
    pub incomplete: bool,
}

pub fn list_matches(conn: &Connection) -> Result<Vec<MatchListRow>> {
    let mut st = conn.prepare(
        "SELECT guid, game_type, map, started_at, finished_at, day_key, aborted, exit_msg,
                game_length, human_players, counted, skip_reason, incomplete
         FROM matches ORDER BY finished_at, guid",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok(MatchListRow {
                guid: r.get(0)?,
                game_type: r.get(1)?,
                map: r.get(2)?,
                started_at: r.get(3)?,
                finished_at: r.get(4)?,
                day_key: r.get(5)?,
                aborted: r.get(6)?,
                exit_msg: r.get(7)?,
                game_length: r.get(8)?,
                human_players: r.get(9)?,
                counted: r.get(10)?,
                skip_reason: r.get(11)?,
                incomplete: r.get(12)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// События после последнего `MATCH_REPORT` — для восстановления live-состояния.
pub fn tail_after_last_report(conn: &Connection) -> Result<Vec<(i64, i64, String)>> {
    let mut st = conn.prepare(
        "SELECT id, received_at, payload FROM raw_events
         WHERE id > COALESCE((SELECT MAX(id) FROM raw_events WHERE type = 'MATCH_REPORT'), 0)
         ORDER BY id",
    )?;
    let rows = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn ping(conn: &Connection) -> Result<()> {
    conn.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?;
    Ok(())
}
