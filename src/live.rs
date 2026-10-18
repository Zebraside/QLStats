//! Live-состояние «Текущий матч» (REQUIREMENTS 3.10, ARCHITECTURE §8).

use std::collections::BTreeMap;
use std::sync::RwLock;

use chrono::{DateTime, Duration, SecondsFormat, TimeZone, Utc};
use chrono_tz::Tz;
use serde::Serialize;
use tokio::sync::watch;

use crate::model::{self, Event, Limits, PlayerRef, SteamId, Team};
use crate::stats::{ENV_MODS, is_team_mode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    Connected,
    Disconnected { since: DateTime<Utc> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Empty,
    Warmup,
    InProgress,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchInfo {
    pub map: Option<String>,
    pub game_type: Option<String>,
    pub factory_title: Option<String>,
    pub limits: Limits,
    pub started_at: DateTime<Utc>,
}

impl MatchInfo {
    fn team_mode(&self) -> bool {
        self.game_type.as_deref().is_some_and(is_team_mode)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivePlayer {
    pub steam_id: String,
    pub name: String,
    pub bot: bool,
    pub team: Team,
    pub score: Option<i32>,
    pub kills: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Inner {
    phase: Phase,
    guid: Option<String>,
    server_title: Option<String>,
    match_info: Option<MatchInfo>,
    /// Ключ — `STEAM_ID`, для ботов — `bot:<NAME>`.
    players: BTreeMap<String, LivePlayer>,
}

/// Чистый автомат состояний. Обёртка с блокировкой — [`LiveHub`].
#[derive(Debug, Clone)]
pub struct LiveState {
    link: Link,
    inner: Inner,
    version: u64,
}

fn player_key(steam: &SteamId, name: &str, bot: bool) -> String {
    if bot || steam.is_bot() {
        format!("bot:{name}")
    } else {
        steam.0.clone()
    }
}

impl LiveState {
    /// Новое состояние: связь считается разорванной с момента `now`.
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            link: Link::Disconnected { since: now },
            inner: Inner::default(),
            version: 1,
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn phase(&self) -> Phase {
        self.inner.phase
    }

    pub fn link(&self) -> Link {
        self.link
    }

    pub fn players(&self) -> impl Iterator<Item = &LivePlayer> {
        self.inner.players.values()
    }

    fn has_humans(&self) -> bool {
        self.inner.players.values().any(|p| !p.bot)
    }

    fn bump_if_changed(&mut self, before: &Inner) {
        if &self.inner != before {
            self.version += 1;
        }
    }

    pub fn apply(&mut self, ev: &Event, at: DateTime<Utc>) {
        let before = self.inner.clone();
        self.apply_inner(ev, at);
        self.bump_if_changed(&before);
    }

    pub fn set_link(&mut self, connected: bool, at: DateTime<Utc>) {
        match (connected, self.link) {
            (true, _) => self.link = Link::Connected,
            (false, Link::Connected) => self.link = Link::Disconnected { since: at },
            (false, Link::Disconnected { .. }) => {}
        }
    }

    /// Правило устаревания: `InProgress` дольше `TIME_LIMIT + 5 мин` закрывается.
    pub fn tick(&mut self, now: DateTime<Utc>) {
        let before = self.inner.clone();
        if self.inner.phase == Phase::InProgress
            && let Some(mi) = &self.inner.match_info
            && let Some(tl) = mi.limits.time_min.filter(|t| *t > 0)
            && now - mi.started_at > Duration::minutes(tl) + Duration::minutes(5)
        {
            tracing::info!(
                guid = self.inner.guid.as_deref().unwrap_or(""),
                "live: матч устарел (TIME_LIMIT + 5 мин), закрыт"
            );
            self.close_match(true);
        }
        self.bump_if_changed(&before);
    }

    /// Флаг `offline` для внешнего состояния.
    pub fn is_offline(&self, now: DateTime<Utc>, offline_secs: u64) -> bool {
        match self.link {
            Link::Connected => false,
            Link::Disconnected { since } => now - since >= Duration::seconds(offline_secs as i64),
        }
    }

    fn close_match(&mut self, remove_bots: bool) {
        let i = &mut self.inner;
        i.match_info = None;
        i.guid = None;
        if remove_bots {
            i.players.retain(|_, p| !p.bot);
        }
        for p in i.players.values_mut() {
            p.score = None;
            p.kills = None;
        }
        i.phase = Phase::Empty;
        self.settle_phase();
    }

    fn clear_all(&mut self) {
        let i = &mut self.inner;
        i.players.clear();
        i.match_info = None;
        i.guid = None;
        i.phase = Phase::Empty;
    }

    /// Вне матча фаза определяется наличием людей.
    fn settle_phase(&mut self) {
        if self.inner.phase != Phase::InProgress {
            self.inner.phase = if self.has_humans() {
                Phase::Warmup
            } else {
                Phase::Empty
            };
        }
    }

    /// Добавляет игрока, если его нет, и обновляет ник. Возвращает ключ.
    fn touch(
        &mut self,
        steam: Option<&SteamId>,
        name: Option<&str>,
        bot: bool,
        team_if_new: Team,
    ) -> Option<String> {
        let steam = steam?;
        let name = name.unwrap_or("").to_string();
        let bot = bot || steam.is_bot();
        let key = player_key(steam, &name, bot);
        let in_progress = self.inner.phase == Phase::InProgress;
        let e = self
            .inner
            .players
            .entry(key.clone())
            .or_insert_with(|| LivePlayer {
                steam_id: steam.0.clone(),
                name: name.clone(),
                bot,
                team: team_if_new,
                score: in_progress.then_some(0),
                kills: in_progress.then_some(0),
            });
        if !name.is_empty() {
            e.name = name;
        }
        Some(key)
    }

    fn touch_ref(&mut self, r: &PlayerRef) -> Option<String> {
        self.touch(
            r.steam_id.as_ref(),
            r.name.as_deref(),
            r.is_bot(),
            r.team.unwrap_or(Team::Free),
        )
    }

    fn apply_inner(&mut self, ev: &Event, at: DateTime<Utc>) {
        let warm = ev.warmup() == Some(true);
        if warm && self.inner.phase == Phase::InProgress {
            if ev.match_guid() != self.inner.guid.as_deref() {
                // MATCH_REPORT потерян: матч закрывается, участники сохраняются.
                tracing::info!(
                    guid = self.inner.guid.as_deref().unwrap_or(""),
                    "live: событие разминки с другим GUID, матч закрыт"
                );
                self.close_match(false);
            } else {
                return;
            }
        }
        match ev {
            Event::MatchStarted(s) => {
                let i = &mut self.inner;
                i.phase = Phase::InProgress;
                i.guid = s.match_guid.clone();
                if s.server_title.is_some() {
                    i.server_title = s.server_title.clone();
                }
                i.match_info = Some(MatchInfo {
                    map: s.map.clone(),
                    game_type: s.game_type.clone(),
                    factory_title: s.factory_title.clone(),
                    limits: s.limits.clone(),
                    started_at: at,
                });
                i.players.clear();
                for p in &s.players {
                    let Some(sid) = &p.steam_id else { continue };
                    let name = p.name.clone().unwrap_or_default();
                    let bot = sid.is_bot();
                    i.players.insert(
                        player_key(sid, &name, bot),
                        LivePlayer {
                            steam_id: sid.0.clone(),
                            name,
                            bot,
                            team: p.team.unwrap_or(Team::Free),
                            score: Some(0),
                            kills: Some(0),
                        },
                    );
                }
            }
            Event::MatchReport(r) => {
                if r.server_title.is_some() {
                    self.inner.server_title = r.server_title.clone();
                }
                // Поздний REPORT другого матча не закрывает текущий.
                let other = self.inner.phase == Phase::InProgress
                    && r.match_guid.is_some()
                    && self.inner.guid.is_some()
                    && r.match_guid != self.inner.guid;
                if !other {
                    self.close_match(true);
                }
            }
            Event::PlayerConnect(e) => {
                self.touch(
                    e.steam_id.as_ref(),
                    e.name.as_deref(),
                    false,
                    Team::Spectator,
                );
            }
            Event::PlayerDisconnect(e) => {
                if let Some(sid) = &e.steam_id {
                    let key = player_key(sid, e.name.as_deref().unwrap_or(""), false);
                    self.inner.players.remove(&key);
                }
                if !self.has_humans() {
                    self.clear_all();
                }
            }
            Event::PlayerSwitchTeam(e) => {
                if let Some(p) = &e.player
                    && let Some(key) = self.touch_ref(p)
                    && let Some(team) = p.team
                    && let Some(lp) = self.inner.players.get_mut(&key)
                {
                    lp.team = team;
                }
            }
            Event::PlayerDeath(d) => {
                let killer = d.killer.as_ref().and_then(|k| self.touch_ref(k));
                let victim = d.victim.as_ref().and_then(|v| self.touch_ref(v));
                if !warm
                    && self.inner.phase == Phase::InProgress
                    && d.match_guid.is_some()
                    && d.match_guid == self.inner.guid
                {
                    self.score_death(d, killer, victim);
                }
            }
            Event::PlayerMedal(e) => {
                self.touch(e.steam_id.as_ref(), e.name.as_deref(), false, Team::Free);
            }
            Event::PlayerStats(s) => {
                // Вышедшие (QUIT = 1) приходят в конце матча — их не возвращаем в список.
                if s.quit != Some(true) {
                    self.touch(
                        s.steam_id.as_ref(),
                        s.name.as_deref(),
                        false,
                        s.team.unwrap_or(Team::Free),
                    );
                }
            }
            Event::PlayerKill(_) | Event::Other { .. } => {}
        }
        self.settle_phase();
    }

    fn score_death(
        &mut self,
        d: &model::PlayerDeath,
        killer: Option<String>,
        victim: Option<String>,
    ) {
        let m = d.mod_.as_deref().unwrap_or("").to_ascii_uppercase();
        if m == "SWITCHTEAM" {
            return;
        }
        let team_mode = self
            .inner
            .match_info
            .as_ref()
            .is_some_and(MatchInfo::team_mode);
        let add = |players: &mut BTreeMap<String, LivePlayer>, k: &str, ds: i32, dk: i32| {
            if let Some(p) = players.get_mut(k) {
                p.score = Some(p.score.unwrap_or(0) + ds);
                p.kills = Some(p.kills.unwrap_or(0) + dk);
            }
        };
        let players = &mut self.inner.players;
        match (&killer, &victim) {
            (Some(k), Some(v)) if k != v => {
                if team_mode {
                    if d.teamkill != Some(true) {
                        add(players, k, 1, 1);
                    }
                } else {
                    add(players, k, 1, 1);
                }
            }
            (Some(k), None) => add(players, k, 1, 1),
            (k, Some(v)) => {
                let self_kill = k.as_ref() == Some(v);
                let env = k.is_none() && ENV_MODS.contains(&m.as_str());
                if !team_mode && (self_kill || m == "SUICIDE" || env) {
                    add(players, v, -1, 0);
                }
            }
            (None, None) => {}
        }
    }

    /// Восстановление из `raw_events` (события после последнего `MATCH_REPORT`).
    pub fn restore<'a>(
        &mut self,
        events: impl IntoIterator<Item = (i64, &'a str)>,
        now: DateTime<Utc>,
    ) {
        for (received_at, payload) in events {
            if let Ok(p) = model::parse(payload) {
                let at = Utc
                    .timestamp_millis_opt(received_at)
                    .single()
                    .unwrap_or(now);
                self.apply(&p.event, at);
            }
        }
        self.tick(now);
    }

    /// Представление для `/api/live`.
    pub fn view(&self, now: DateTime<Utc>, offline_secs: u64, tz: Tz) -> LiveView {
        let fmt = |t: DateTime<Utc>| {
            t.with_timezone(&tz)
                .to_rfc3339_opts(SecondsFormat::Secs, false)
        };
        let mut v = LiveView {
            generated_at: fmt(now),
            state: "empty",
            server_title: self.inner.server_title.clone(),
            offline_since: None,
            match_: None,
            players: Vec::new(),
            spectators: 0,
            score_approximate: true,
        };
        if self.is_offline(now, offline_secs) {
            v.state = "offline";
            if let Link::Disconnected { since } = self.link {
                v.offline_since = Some(fmt(since));
            }
            return v;
        }
        if !self.has_humans() {
            return v;
        }
        v.state = match self.inner.phase {
            Phase::InProgress => "in_progress",
            _ => "warmup",
        };
        if self.inner.phase == Phase::InProgress
            && let Some(mi) = &self.inner.match_info
        {
            v.match_ = Some(MatchView::new(
                self.inner.guid.clone(),
                mi,
                fmt(mi.started_at),
            ));
        }
        let team_mode = v.match_.as_ref().is_some_and(|m| m.team_mode);
        for p in self.inner.players.values() {
            if p.team == Team::Spectator {
                v.spectators += 1;
                continue;
            }
            v.players.push(PlayerView {
                steam_id: p.steam_id.clone(),
                name: p.name.clone(),
                bot: p.bot,
                team: if team_mode { p.team.as_str() } else { "free" },
                score: p.score,
                kills: p.kills,
            });
        }
        v.players.sort_by(|a, b| {
            b.score
                .unwrap_or(i32::MIN)
                .cmp(&a.score.unwrap_or(i32::MIN))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        v
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LiveLimits {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frag: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_min: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MatchView {
    pub guid: Option<String>,
    pub map: Option<String>,
    pub game_type: Option<String>,
    pub factory_title: Option<String>,
    pub team_mode: bool,
    pub started_at: String,
    pub limits: LiveLimits,
}

impl MatchView {
    fn new(guid: Option<String>, mi: &MatchInfo, started_at: String) -> Self {
        let gt = mi
            .game_type
            .clone()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let nz = |v: Option<i64>| v.filter(|x| *x > 0);
        let l = &mi.limits;
        Self {
            guid,
            map: mi.map.clone(),
            game_type: mi.game_type.clone(),
            factory_title: mi.factory_title.clone(),
            team_mode: mi.team_mode(),
            started_at,
            limits: LiveLimits {
                frag: nz(l.frag),
                time_min: nz(l.time_min),
                capture: nz(l.capture).filter(|_| matches!(gt.as_str(), "CTF" | "ONEFLAG")),
                round: nz(l.round).filter(|_| matches!(gt.as_str(), "CA" | "FT" | "AD")),
                score: nz(l.score).filter(|_| gt == "DOM"),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PlayerView {
    pub steam_id: String,
    pub name: String,
    pub bot: bool,
    pub team: &'static str,
    pub score: Option<i32>,
    pub kills: Option<i32>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LiveView {
    pub generated_at: String,
    pub state: &'static str,
    pub server_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offline_since: Option<String>,
    #[serde(rename = "match")]
    pub match_: Option<MatchView>,
    pub players: Vec<PlayerView>,
    pub spectators: u32,
    pub score_approximate: bool,
}

/// Потокобезопасная обёртка: `RwLock` + уведомление через `watch`.
pub struct LiveHub {
    state: RwLock<LiveState>,
    notify: watch::Sender<u64>,
    pub offline_secs: u64,
    pub tz: Tz,
}

impl LiveHub {
    pub fn new(state: LiveState, offline_secs: u64, tz: Tz) -> Self {
        let (notify, _) = watch::channel(state.version());
        Self {
            state: RwLock::new(state),
            notify,
            offline_secs,
            tz,
        }
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, LiveState> {
        self.state.write().unwrap_or_else(|e| e.into_inner())
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, LiveState> {
        self.state.read().unwrap_or_else(|e| e.into_inner())
    }

    fn changed(&self, v: u64) {
        self.notify.send_replace(v);
    }

    pub fn apply(&self, ev: &Event, at: DateTime<Utc>) {
        let v = {
            let mut s = self.write();
            s.apply(ev, at);
            s.version()
        };
        if *self.notify.borrow() != v {
            self.changed(v);
        }
    }

    pub fn set_link(&self, connected: bool, at: DateTime<Utc>) {
        self.write().set_link(connected, at);
        self.notify.send_modify(|_| {});
    }

    pub fn tick(&self, now: DateTime<Utc>) {
        let v = {
            let mut s = self.write();
            s.tick(now);
            s.version()
        };
        if *self.notify.borrow() != v {
            self.changed(v);
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.notify.subscribe()
    }

    /// Ключ кэша: версия состояния, флаг offline и начало текущего обрыва.
    pub fn cache_key(&self, now: DateTime<Utc>) -> (u64, bool, i64) {
        let s = self.read();
        let offline = s.is_offline(now, self.offline_secs);
        let since = match (offline, s.link()) {
            (true, Link::Disconnected { since }) => since.timestamp(),
            _ => 0,
        };
        (s.version(), offline, since)
    }

    pub fn view(&self, now: DateTime<Utc>) -> LiveView {
        self.read().view(now, self.offline_secs, self.tz)
    }

    pub fn snapshot(&self) -> LiveState {
        self.read().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn tz() -> Tz {
        "Europe/Moscow".parse().unwrap()
    }

    /// Строки фикстуры: (received_at, событие).
    fn load(name: &str) -> Vec<(DateTime<Utc>, Event, String)> {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let v: Value = serde_json::from_str(l).unwrap();
                let at = DateTime::parse_from_rfc3339(v["received_at"].as_str().unwrap())
                    .unwrap()
                    .with_timezone(&Utc);
                let kind = v["type"].as_str().unwrap().to_string();
                (at, model::parse_data(&kind, v["data"].clone()), kind)
            })
            .collect()
    }

    fn connected(at: DateTime<Utc>) -> LiveState {
        let mut s = LiveState::new(at);
        s.set_link(true, at);
        s
    }

    fn state_at(s: &LiveState, at: DateTime<Utc>) -> &'static str {
        s.view(at, 15, tz()).state
    }

    #[test]
    fn real_match_end_goes_warmup_then_empty() {
        let evs = load("sample-long.jsonl");
        let mut s = connected(evs[0].0);
        // Синтетический старт матча из записи (сама запись начинается с середины).
        let start = model::parse_data(
            "MATCH_STARTED",
            serde_json::json!({"MATCH_GUID":"18e94174-2862-44c2-a808-3c6f7eb0f0a5","GAME_TYPE":"FFA",
              "MAP":"hellsgate","FACTORY_TITLE":"Free For All","FRAG_LIMIT":50,"TIME_LIMIT":15,
              "PLAYERS":[{"NAME":"Saint_P","STEAM_ID":"76561198182889645","TEAM":0},{"NAME":"Ptica","STEAM_ID":"0","TEAM":0}]}),
        );
        s.apply(&start, evs[0].0);
        assert_eq!(state_at(&s, evs[0].0), "in_progress");
        let mut saw_warmup_after_report = false;
        for (at, ev, kind) in &evs {
            s.apply(ev, *at);
            if kind == "MATCH_REPORT" && !saw_warmup_after_report {
                // Человек ещё на сервере, боты сброшены.
                assert_eq!(state_at(&s, *at), "warmup");
                let v = s.view(*at, 15, tz());
                assert_eq!(v.players.len(), 1);
                assert!(!v.players[0].bot);
                assert_eq!(v.players[0].score, None);
                assert!(v.match_.is_none());
                saw_warmup_after_report = true;
            }
            if kind == "PLAYER_DISCONNECT" && at.to_rfc3339().starts_with("2026-10-18T17:37:08") {
                assert_eq!(state_at(&s, *at), "empty");
                break;
            }
        }
        assert!(saw_warmup_after_report);
    }

    #[test]
    fn warmup_connect_and_switchteam() {
        let evs = load("sample-long.jsonl");
        let mut s = connected(evs[0].0);
        let idx = evs
            .iter()
            .position(|(_, _, k)| k == "PLAYER_CONNECT")
            .unwrap();
        // CONNECT: игрок пока зритель.
        s.apply(&evs[idx].1, evs[idx].0);
        let v = s.view(evs[idx].0, 15, tz());
        assert_eq!(v.state, "warmup");
        assert_eq!(v.players.len(), 0);
        assert_eq!(v.spectators, 1);
        // SWITCHTEAM SPECTATOR -> FREE
        s.apply(&evs[idx + 1].1, evs[idx + 1].0);
        let v = s.view(evs[idx + 1].0, 15, tz());
        assert_eq!(v.state, "warmup");
        assert_eq!(v.players.len(), 1);
        assert_eq!(v.players[0].team, "free");
        assert_eq!(v.players[0].score, None);
        assert_eq!(v.players[0].name, "Drowsy");
        assert_eq!(v.spectators, 0);
    }

    #[test]
    fn match_started_with_players_and_bot() {
        let evs = load("sample-long.jsonl");
        let (at, ev, _) = evs.iter().find(|e| e.2 == "MATCH_STARTED").unwrap();
        let mut s = connected(*at);
        s.apply(ev, *at);
        let v = s.view(*at, 15, tz());
        assert_eq!(v.state, "in_progress");
        let m = v.match_.unwrap();
        assert_eq!(m.map.as_deref(), Some("tornado"));
        assert_eq!(m.limits.frag, Some(50));
        assert_eq!(m.limits.time_min, Some(15));
        assert_eq!(m.limits.capture, None);
        assert_eq!(m.limits.round, None);
        assert_eq!(v.players.len(), 2);
        assert!(v.players.iter().any(|p| p.bot && p.name == "Ptica"));
        assert!(v.players.iter().all(|p| p.score == Some(0)));
    }

    fn death(
        guid: &str,
        killer: Option<(&str, &str, bool)>,
        victim: (&str, &str, bool),
        m: &str,
    ) -> Event {
        let blk = |(id, n, b): (&str, &str, bool)| serde_json::json!({"STEAM_ID": id, "NAME": n, "BOT": b, "TEAM": 0});
        let mut d = serde_json::json!({"MATCH_GUID": guid, "MOD": m, "WARMUP": false, "VICTIM": blk(victim), "TIME": 1});
        if let Some(k) = killer {
            d["KILLER"] = blk(k);
        }
        model::parse_data("PLAYER_DEATH", d)
    }

    #[test]
    fn live_score_rules() {
        let t0 = Utc::now();
        let mut s = connected(t0);
        s.apply(
            &model::parse_data(
                "MATCH_STARTED",
                serde_json::json!({"MATCH_GUID":"g","GAME_TYPE":"FFA","PLAYERS":[
                    {"NAME":"A","STEAM_ID":"1","TEAM":0},{"NAME":"Bot","STEAM_ID":"0","TEAM":0}]}),
            ),
            t0,
        );
        let a = ("1", "A", false);
        let bot = ("0", "Bot", true);
        s.apply(&death("g", Some(a), bot, "RAILGUN"), t0); // +1
        s.apply(&death("g", Some(a), bot, "RAILGUN"), t0); // +1
        s.apply(&death("g", Some(a), a, "ROCKET_SPLASH"), t0); // -1
        s.apply(&death("g", None, a, "LAVA"), t0); // -1
        s.apply(&death("g", Some(a), a, "SWITCHTEAM"), t0); // 0
        s.apply(&death("g", Some(bot), a, "RAILGUN"), t0); // бот +1
        s.apply(&death("other", Some(a), bot, "RAILGUN"), t0); // чужой GUID — не считается
        let v = s.view(t0, 15, tz());
        let pa = v.players.iter().find(|p| p.name == "A").unwrap();
        assert_eq!(pa.score, Some(0));
        assert_eq!(pa.kills, Some(2));
        let pb = v.players.iter().find(|p| p.name == "Bot").unwrap();
        assert_eq!(pb.score, Some(1));
    }

    #[test]
    fn live_score_matches_final_score_in_real_record() {
        // Матч 3f8e4859: MATCH_STARTED, убийство человека ботом, итоговые PLAYER_STATS.
        let evs = load("sample-long.jsonl");
        let start = evs.iter().rposition(|e| e.2 == "MATCH_STARTED").unwrap();
        let finals: BTreeMap<String, i64> = evs[start..]
            .iter()
            .filter_map(|(_, ev, _)| match ev {
                Event::PlayerStats(st) => Some((st.name.clone()?, st.score?)),
                _ => None,
            })
            .collect();
        let mut s = connected(evs[start].0);
        let first_stats = evs[start..]
            .iter()
            .position(|e| e.2 == "PLAYER_STATS")
            .unwrap()
            + start;
        for (at, ev, _) in &evs[start..first_stats] {
            s.apply(ev, *at);
        }
        let v = s.view(evs[first_stats].0, 15, tz());
        assert_eq!(v.players.len(), 2);
        for p in &v.players {
            let fin = finals[&p.name];
            let diff = (p.score.unwrap() as i64 - fin).abs();
            assert!(diff <= 2, "{}: live {:?} vs итог {fin}", p.name, p.score);
        }
        for (at, ev, _) in &evs[first_stats..] {
            s.apply(ev, *at);
        }
        // Конец записи: люди ушли.
        assert_eq!(state_at(&s, evs.last().unwrap().0), "empty");
    }

    #[test]
    fn lost_report_and_staleness() {
        let t0 = DateTime::parse_from_rfc3339("2026-10-18T17:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut s = connected(t0);
        let start = model::parse_data(
            "MATCH_STARTED",
            serde_json::json!({"MATCH_GUID":"g1","GAME_TYPE":"FFA","TIME_LIMIT":10,"PLAYERS":[
                {"NAME":"A","STEAM_ID":"1","TEAM":0}]}),
        );
        s.apply(&start, t0);
        assert_eq!(s.phase(), Phase::InProgress);
        let warm = model::parse_data(
            "PLAYER_CONNECT",
            serde_json::json!({"MATCH_GUID":"g2","NAME":"B","STEAM_ID":"2","WARMUP":true}),
        );
        s.apply(&warm, t0 + Duration::seconds(5));
        assert_eq!(s.phase(), Phase::Warmup);
        let v = s.view(t0, 15, tz());
        assert_eq!(v.state, "warmup");
        assert!(v.players.iter().all(|p| p.score.is_none()));

        // Устаревание.
        s.apply(&start, t0);
        s.tick(t0 + Duration::minutes(14));
        assert_eq!(s.phase(), Phase::InProgress);
        s.tick(t0 + Duration::minutes(15) + Duration::seconds(1));
        assert_eq!(s.phase(), Phase::Warmup);
    }

    #[test]
    fn link_offline_and_back() {
        let t0 = Utc::now();
        let mut s = connected(t0);
        s.apply(
            &model::parse_data(
                "MATCH_STARTED",
                serde_json::json!({"MATCH_GUID":"g","GAME_TYPE":"FFA","PLAYERS":[{"NAME":"A","STEAM_ID":"1","TEAM":0}]}),
            ),
            t0,
        );
        s.set_link(false, t0);
        assert_eq!(state_at(&s, t0 + Duration::seconds(5)), "in_progress");
        let v = s.view(t0 + Duration::seconds(16), 15, tz());
        assert_eq!(v.state, "offline");
        assert!(v.offline_since.is_some());
        assert!(v.players.is_empty());
        s.set_link(true, t0 + Duration::seconds(20));
        assert_eq!(state_at(&s, t0 + Duration::seconds(21)), "in_progress");
        // На старте offline отсчитывается от момента запуска.
        let fresh = LiveState::new(t0);
        assert_eq!(state_at(&fresh, t0 + Duration::seconds(3)), "empty");
        assert_eq!(state_at(&fresh, t0 + Duration::seconds(15)), "offline");
    }

    #[test]
    fn restore_matches_incremental() {
        let path = format!(
            "{}/tests/fixtures/sample-long.jsonl",
            env!("CARGO_MANIFEST_DIR")
        );
        let lines: Vec<(i64, String)> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|l| {
                let v: Value = serde_json::from_str(l).unwrap();
                let at = DateTime::parse_from_rfc3339(v["received_at"].as_str().unwrap())
                    .unwrap()
                    .timestamp_millis();
                let payload = serde_json::json!({"TYPE": v["type"], "DATA": v["data"]}).to_string();
                (at, payload)
            })
            .collect();
        // Хвост после MATCH_STARTED последнего матча, но до его MATCH_REPORT.
        let cut = lines.len() - 3;
        let now = Utc.timestamp_millis_opt(lines[cut - 1].0).unwrap();
        let mut inc = LiveState::new(now);
        for (at, p) in &lines[..cut] {
            let e = model::parse(p).unwrap();
            inc.apply(&e.event, Utc.timestamp_millis_opt(*at).unwrap());
        }
        inc.tick(now);
        let mut rest = LiveState::new(now);
        rest.restore(lines[..cut].iter().map(|(a, p)| (*a, p.as_str())), now);
        assert_eq!(inc.inner, rest.inner);
        assert_eq!(rest.phase(), Phase::InProgress);
    }
}
