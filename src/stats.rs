//! Чистые правила учёта (REQUIREMENTS, раздел 3). Ввода-вывода нет.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;

use crate::model::{MatchReport, MatchStarted, PlayerDeath, PlayerStats, SteamId};

/// Режимы, которые засчитываются.
pub const COUNTED_MODES: &[&str] = &["FFA", "DUEL"];
/// Командные режимы.
pub const TEAM_MODES: &[&str] = &[
    "CA", "TDM", "CTF", "ONEFLAG", "FT", "AD", "DOM", "RR", "HAR",
];
/// `MOD` смерти от окружения.
pub const ENV_MODS: &[&str] = &[
    "FALLING",
    "LAVA",
    "SLIME",
    "WATER",
    "CRUSH",
    "HURT",
    "TRIGGER_HURT",
    "UNKNOWN",
];

/// Канонические ключи оружия (как в `WEAPONS` у `PLAYER_STATS`) в порядке тай-брейка
/// «любимого оружия»: при равенстве убийств побеждает оружие раньше в списке.
pub const WEAPON_ORDER: &[&str] = &[
    "GAUNTLET",
    "MACHINEGUN",
    "SHOTGUN",
    "GRENADE",
    "ROCKET",
    "LIGHTNING",
    "RAILGUN",
    "PLASMA",
    "BFG",
    "HMG",
    "CHAINGUN",
    "NAILGUN",
    "PROXMINE",
];

/// `MOD` (причина смерти) → канонический ключ оружия. Прочие `MOD` (TELEFRAG, KAMIKAZE,
/// UNKNOWN и т.п.) оружию не приписываются.
pub fn weapon_for_mod(m: &str) -> Option<&'static str> {
    Some(match m.to_ascii_uppercase().as_str() {
        "GAUNTLET" => "GAUNTLET",
        "MACHINEGUN" => "MACHINEGUN",
        "SHOTGUN" => "SHOTGUN",
        "GRENADE" | "GRENADE_SPLASH" => "GRENADE",
        "ROCKET" | "ROCKET_SPLASH" => "ROCKET",
        "LIGHTNING" => "LIGHTNING",
        "RAILGUN" => "RAILGUN",
        "PLASMA" | "PLASMA_SPLASH" => "PLASMA",
        "BFG" | "BFG_SPLASH" => "BFG",
        "HMG" => "HMG",
        "CHAINGUN" => "CHAINGUN",
        "NAILGUN" => "NAILGUN",
        // В ql-stats-model — PROXMINE/PROXMINE_SPLASH; PROXIMITY_MINE — вариант из постановки.
        "PROXMINE" | "PROXMINE_SPLASH" | "PROXIMITY_MINE" => "PROXMINE",
        _ => return None,
    })
}

/// Любимое оружие по 3.x: больше всего убийств, при равенстве — раньше в [`WEAPON_ORDER`].
/// Ключи вне `WEAPON_ORDER` и нулевые счётчики не рассматриваются.
pub fn favorite_weapon<'a>(
    kills: impl IntoIterator<Item = (&'a str, i64)>,
) -> Option<(&'static str, i64)> {
    let mut per = [0i64; WEAPON_ORDER.len()];
    for (w, n) in kills {
        if let Some(i) = WEAPON_ORDER.iter().position(|x| *x == w) {
            per[i] += n;
        }
    }
    let mut best: Option<(&'static str, i64)> = None;
    for (i, n) in per.iter().enumerate() {
        if *n > 0 && best.is_none_or(|(_, b)| *n > b) {
            best = Some((WEAPON_ORDER[i], *n));
        }
    }
    best
}

pub fn is_team_mode(game_type: &str) -> bool {
    TEAM_MODES.contains(&game_type.to_ascii_uppercase().as_str())
}

// ---------------------------------------------------------------- deaths

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Kill,
    Suicide,
    Environment,
    Ignored,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Kill => "kill",
            Category::Suicide => "suicide",
            Category::Environment => "environment",
            Category::Ignored => "ignored",
        }
    }
}

/// Почему смерть получила `ignored`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IgnoreReason {
    SwitchTeam,
    NoVictim,
    BotVictim,
    BotKiller,
    /// Правило 7: оружейный `MOD` без `KILLER` и прочее — пишется в лог.
    Unclassified,
}

/// Классификация `PLAYER_DEATH` по таблице REQUIREMENTS 3.4 (первое подходящее правило).
pub fn classify_death(d: &PlayerDeath) -> (Category, Option<IgnoreReason>) {
    let m = d.mod_.as_deref().unwrap_or("").to_ascii_uppercase();
    if m == "SWITCHTEAM" {
        return (Category::Ignored, Some(IgnoreReason::SwitchTeam));
    }
    let Some(victim) = d.victim.as_ref() else {
        return (Category::Ignored, Some(IgnoreReason::NoVictim));
    };
    if victim.is_bot() {
        return (Category::Ignored, Some(IgnoreReason::BotVictim));
    }
    let killer = d
        .killer
        .as_ref()
        .filter(|k| k.steam_id.is_some() || k.name.is_some());
    if let Some(k) = killer {
        if k.is_bot() {
            return (Category::Ignored, Some(IgnoreReason::BotKiller));
        }
        if k.steam_id.is_some() && k.steam_id != victim.steam_id {
            return (Category::Kill, None);
        }
    }
    let self_kill = killer.is_some_and(|k| k.steam_id == victim.steam_id);
    if m == "SUICIDE" || self_kill {
        return (Category::Suicide, None);
    }
    if killer.is_none() && ENV_MODS.contains(&m.as_str()) {
        return (Category::Environment, None);
    }
    (Category::Ignored, Some(IgnoreReason::Unclassified))
}

/// `day_key` — календарный день в часовом поясе `tz`.
pub fn day_key(ts_ms: i64, tz: Tz) -> String {
    let utc: DateTime<Utc> = Utc.timestamp_millis_opt(ts_ms).single().unwrap_or_default();
    utc.with_timezone(&tz).format("%Y-%m-%d").to_string()
}

// ---------------------------------------------------------------- per-match computation

#[derive(Debug, Clone)]
pub struct DeathInput {
    pub raw_id: i64,
    pub ev: PlayerDeath,
}

/// Всё, что `finalize_match` собрал из `raw_events` по одному GUID.
#[derive(Debug, Clone)]
pub struct MatchInput {
    pub guid: String,
    /// `received_at` и данные `MATCH_STARTED`, если он был до REPORT.
    pub started: Option<(i64, MatchStarted)>,
    pub report: MatchReport,
    pub finished_at: i64,
    /// `PLAYER_STATS` без warmup в порядке получения.
    pub stats: Vec<PlayerStats>,
    /// `PLAYER_DEATH` без warmup в порядке получения.
    pub deaths: Vec<DeathInput>,
    pub incomplete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    Aborted,
    NoStart,
    TeamMode,
    Race,
    /// Режим не из известных списков (отклонение от схемы, см. README).
    UnsupportedMode,
    Lt2Humans,
}

impl SkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            SkipReason::Aborted => "aborted",
            SkipReason::NoStart => "no_start",
            SkipReason::TeamMode => "team_mode",
            SkipReason::Race => "race",
            SkipReason::UnsupportedMode => "unsupported_mode",
            SkipReason::Lt2Humans => "lt2_humans",
        }
    }
}

/// Критерии 3.1 (без проверки дубликата — она в store).
pub fn skip_reason(
    aborted: bool,
    has_start: bool,
    game_type: &str,
    humans: usize,
) -> Option<SkipReason> {
    let gt = game_type.to_ascii_uppercase();
    if aborted {
        Some(SkipReason::Aborted)
    } else if !has_start {
        Some(SkipReason::NoStart)
    } else if TEAM_MODES.contains(&gt.as_str()) {
        Some(SkipReason::TeamMode)
    } else if gt == "RACE" {
        Some(SkipReason::Race)
    } else if !COUNTED_MODES.contains(&gt.as_str()) {
        Some(SkipReason::UnsupportedMode)
    } else if humans < 2 {
        Some(SkipReason::Lt2Humans)
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchRow {
    pub guid: String,
    pub game_type: Option<String>,
    pub map: Option<String>,
    pub started_at: Option<i64>,
    pub finished_at: i64,
    pub aborted: bool,
    pub exit_msg: Option<String>,
    pub game_length: Option<i64>,
    pub human_players: i64,
    pub counted: bool,
    pub skip_reason: Option<SkipReason>,
    pub incomplete: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MatchPlayerRow {
    pub steam_id: String,
    pub name: Option<String>,
    pub has_stats: bool,
    pub score: Option<i64>,
    pub rank: Option<i64>,
    pub win_raw: Option<i64>,
    pub quit: Option<bool>,
    pub max_streak_reported: Option<i64>,
    pub weapons_json: Option<String>,
    /// Засчитанные убийства по оружию из `MOD`: `{"RAILGUN":12,...}`; `None`, если их нет.
    pub weapon_kills_json: Option<String>,
    pub kills: i64,
    pub deaths: i64,
    pub suicides: i64,
    pub env_deaths: i64,
    pub best_streak: i64,
    pub is_winner: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WeaponRow {
    pub steam_id: String,
    pub weapon: String,
    pub shots: i64,
    pub hits: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeathRow {
    pub raw_id: i64,
    pub time: Option<i64>,
    pub victim: Option<String>,
    pub killer: Option<String>,
    pub mod_: Option<String>,
    pub category: Category,
    pub ignore_reason: Option<IgnoreReason>,
}

#[derive(Debug, Clone)]
pub struct MatchOutcome {
    pub row: MatchRow,
    pub players: Vec<MatchPlayerRow>,
    pub weapons: Vec<WeaponRow>,
    pub deaths: Vec<DeathRow>,
}

#[derive(Default)]
struct Acc {
    kills: i64,
    deaths: i64,
    suicides: i64,
    env: i64,
    cur: i64,
    best: i64,
    weapon_kills: BTreeMap<&'static str, i64>,
}

fn sid(s: &Option<SteamId>) -> Option<String> {
    s.as_ref().map(|s| s.0.clone())
}

/// Победители по 3.6. Возвращает множество STEAM_ID людей.
pub fn winners(game_type: &str, stats: &[&PlayerStats]) -> BTreeSet<String> {
    let humans: Vec<&&PlayerStats> = stats
        .iter()
        .filter(|s| s.steam_id.as_ref().is_some_and(|x| !x.is_bot()))
        .collect();
    let gt = game_type.to_ascii_uppercase();
    if gt == "DUEL" {
        let any_win = stats.iter().any(|s| s.win.is_some());
        if any_win {
            return humans
                .iter()
                .filter(|s| s.win == Some(1))
                .filter_map(|s| sid(&s.steam_id))
                .collect();
        }
        // WIN нет: победитель — максимальный SCORE среди всех (бот тоже может выиграть).
        let max = stats.iter().filter_map(|s| s.score).max();
        return match max {
            None => BTreeSet::new(),
            Some(m) => humans
                .iter()
                .filter(|s| s.score == Some(m))
                .filter_map(|s| sid(&s.steam_id))
                .collect(),
        };
    }
    // FFA: кандидаты — люди с QUIT = 0.
    let cands: Vec<&&&PlayerStats> = humans.iter().filter(|s| s.quit != Some(true)).collect();
    let max = cands.iter().filter_map(|s| s.score).max();
    match max {
        None => BTreeSet::new(),
        Some(m) => cands
            .iter()
            .filter(|s| s.score == Some(m))
            .filter_map(|s| sid(&s.steam_id))
            .collect(),
    }
}

/// Средняя точность по 3.7: по каждому оружию ΣH/ΣS при ΣS > 0, затем среднее.
pub fn average_accuracy<'a>(rows: impl IntoIterator<Item = (&'a str, i64, i64)>) -> Option<f64> {
    let mut per: BTreeMap<&str, (i64, i64)> = BTreeMap::new();
    for (w, s, h) in rows {
        let e = per.entry(w).or_default();
        e.0 += s;
        e.1 += h;
    }
    let accs: Vec<f64> = per
        .values()
        .filter(|(s, _)| *s > 0)
        .map(|(s, h)| *h as f64 / *s as f64)
        .collect();
    if accs.is_empty() {
        None
    } else {
        Some(accs.iter().sum::<f64>() / accs.len() as f64)
    }
}

/// Полный расчёт одного матча.
pub fn compute_match(input: &MatchInput) -> MatchOutcome {
    let game_type = input
        .report
        .game_type
        .clone()
        .or_else(|| input.started.as_ref().and_then(|s| s.1.game_type.clone()));
    let map = input
        .report
        .map
        .clone()
        .or_else(|| input.started.as_ref().and_then(|s| s.1.map.clone()));

    // Последний PLAYER_STATS на игрока-человека.
    let mut stats_by: BTreeMap<String, &PlayerStats> = BTreeMap::new();
    for s in &input.stats {
        if let Some(id) = s.steam_id.as_ref().filter(|x| !x.is_bot()) {
            stats_by.insert(id.0.clone(), s);
        }
    }

    // Смерти по порядку TIME, затем порядку получения.
    let mut deaths: Vec<&DeathInput> = input.deaths.iter().collect();
    deaths.sort_by_key(|d| (d.ev.time.unwrap_or(i64::MAX), d.raw_id));

    let mut participants: BTreeSet<String> = stats_by.keys().cloned().collect();
    let mut names: HashMap<String, String> = HashMap::new();
    let mut acc: HashMap<String, Acc> = HashMap::new();
    let mut death_rows = Vec::with_capacity(deaths.len());

    for d in deaths {
        let ev = &d.ev;
        for b in [&ev.killer, &ev.victim].into_iter().flatten() {
            if !b.is_bot()
                && let Some(id) = &b.steam_id
            {
                participants.insert(id.0.clone());
                if let Some(n) = &b.name {
                    names.insert(id.0.clone(), n.clone());
                }
            }
        }
        let (cat, reason) = classify_death(ev);
        let victim = ev.victim.as_ref().and_then(|v| sid(&v.steam_id));
        let killer = ev.killer.as_ref().and_then(|k| sid(&k.steam_id));
        let victim_human = ev.victim.as_ref().is_some_and(|v| !v.is_bot());
        match cat {
            Category::Kill => {
                if let Some(k) = &killer {
                    let a = acc.entry(k.clone()).or_default();
                    a.kills += 1;
                    if let Some(w) = ev.mod_.as_deref().and_then(weapon_for_mod) {
                        *a.weapon_kills.entry(w).or_default() += 1;
                    }
                    a.cur += 1;
                    a.best = a.best.max(a.cur);
                }
                if let Some(v) = &victim {
                    let a = acc.entry(v.clone()).or_default();
                    a.deaths += 1;
                    a.cur = 0;
                }
            }
            Category::Suicide | Category::Environment => {
                if let Some(v) = &victim {
                    let a = acc.entry(v.clone()).or_default();
                    if cat == Category::Suicide {
                        a.suicides += 1;
                    } else {
                        a.env += 1;
                    }
                    a.cur = 0;
                }
            }
            Category::Ignored => {
                if reason == Some(IgnoreReason::Unclassified) {
                    tracing::warn!(
                        guid = %input.guid,
                        raw_id = d.raw_id,
                        r#mod = ev.mod_.as_deref().unwrap_or(""),
                        "смерть не классифицирована (правило 7), игнорируется"
                    );
                }
                // Любая собственная смерть человека, кроме SWITCHTEAM, обнуляет стрик.
                if victim_human
                    && reason != Some(IgnoreReason::SwitchTeam)
                    && let Some(v) = &victim
                {
                    acc.entry(v.clone()).or_default().cur = 0;
                }
            }
        }
        death_rows.push(DeathRow {
            raw_id: d.raw_id,
            time: ev.time,
            victim,
            killer: if ev.killer.is_some() { killer } else { None },
            mod_: ev.mod_.clone(),
            category: cat,
            ignore_reason: reason,
        });
    }

    let gt = game_type.clone().unwrap_or_default();
    // Люди — по одному последнему STATS; боты передаются тоже (нужны для DUEL без WIN).
    let mut stat_refs: Vec<&PlayerStats> = stats_by.values().copied().collect();
    stat_refs.extend(
        input
            .stats
            .iter()
            .filter(|s| s.steam_id.as_ref().is_some_and(SteamId::is_bot)),
    );
    let win_set = winners(&gt, &stat_refs);

    let aborted = input.report.aborted.unwrap_or(false);
    let humans = participants.len();
    let skip = skip_reason(aborted, input.started.is_some(), &gt, humans);

    let mut players = Vec::new();
    let mut weapons = Vec::new();
    for id in &participants {
        let st = stats_by.get(id).copied();
        let a = acc.remove(id).unwrap_or_default();
        if let Some(st) = st
            && let Some(ws) = &st.weapons
        {
            for (w, v) in ws {
                weapons.push(WeaponRow {
                    steam_id: id.clone(),
                    weapon: w.clone(),
                    shots: v.shots.unwrap_or(0),
                    hits: v.hits.unwrap_or(0),
                });
            }
        }
        players.push(MatchPlayerRow {
            steam_id: id.clone(),
            name: st
                .and_then(|s| s.name.clone())
                .or_else(|| names.get(id).cloned()),
            has_stats: st.is_some(),
            score: st.and_then(|s| s.score),
            rank: st.and_then(|s| s.rank),
            win_raw: st.and_then(|s| s.win),
            quit: st.and_then(|s| s.quit),
            max_streak_reported: st.and_then(|s| s.max_streak),
            weapons_json: st.and_then(|s| s.weapons.as_ref()).map(weapons_to_json),
            weapon_kills_json: (!a.weapon_kills.is_empty())
                .then(|| serde_json::to_string(&a.weapon_kills).unwrap_or_default()),
            kills: a.kills,
            deaths: a.deaths,
            suicides: a.suicides,
            env_deaths: a.env,
            best_streak: a.best,
            is_winner: win_set.contains(id),
        });
    }

    MatchOutcome {
        row: MatchRow {
            guid: input.guid.clone(),
            game_type,
            map,
            started_at: input.started.as_ref().map(|s| s.0),
            finished_at: input.finished_at,
            aborted,
            exit_msg: input.report.exit_msg.clone(),
            game_length: input.report.game_length,
            human_players: humans as i64,
            counted: skip.is_none(),
            skip_reason: skip,
            incomplete: input.incomplete,
        },
        players,
        weapons,
        deaths: death_rows,
    }
}

fn weapons_to_json(w: &BTreeMap<String, crate::model::WeaponStats>) -> String {
    let m: serde_json::Map<String, serde_json::Value> = w
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::json!({"S": v.shots, "H": v.hits})))
        .collect();
    serde_json::Value::Object(m).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PlayerRef, WeaponStats};

    fn p(id: &str, name: &str) -> PlayerRef {
        PlayerRef {
            name: Some(name.into()),
            steam_id: Some(SteamId(id.into())),
            bot: Some(id == "0"),
            ..Default::default()
        }
    }

    fn death(killer: Option<PlayerRef>, victim: PlayerRef, m: &str, time: i64) -> PlayerDeath {
        PlayerDeath {
            match_guid: Some("g".into()),
            time: Some(time),
            mod_: Some(m.into()),
            warmup: Some(false),
            killer,
            victim: Some(victim),
            ..Default::default()
        }
    }

    #[test]
    fn classification_table() {
        let a = p("1", "A");
        let b = p("2", "B");
        let bot = p("0", "Bot");
        let c = |d: &PlayerDeath| classify_death(d).0;
        // 1 SWITCHTEAM
        assert_eq!(
            classify_death(&death(Some(a.clone()), a.clone(), "SWITCHTEAM", 1)),
            (Category::Ignored, Some(IgnoreReason::SwitchTeam))
        );
        // 2 bot victim
        assert_eq!(
            classify_death(&death(Some(a.clone()), bot.clone(), "RAILGUN", 1)).1,
            Some(IgnoreReason::BotVictim)
        );
        // 3 bot killer
        assert_eq!(
            classify_death(&death(Some(bot.clone()), a.clone(), "RAILGUN", 1)).1,
            Some(IgnoreReason::BotKiller)
        );
        // 4 kill
        assert_eq!(
            c(&death(Some(a.clone()), b.clone(), "RAILGUN", 1)),
            Category::Kill
        );
        // 5 self splash and /kill
        assert_eq!(
            c(&death(Some(a.clone()), a.clone(), "ROCKET_SPLASH", 1)),
            Category::Suicide
        );
        assert_eq!(c(&death(None, a.clone(), "SUICIDE", 1)), Category::Suicide);
        // 6 environment
        assert_eq!(c(&death(None, a.clone(), "LAVA", 1)), Category::Environment);
        assert_eq!(
            c(&death(None, a.clone(), "UNKNOWN", 1)),
            Category::Environment
        );
        // 7 weapon without killer
        assert_eq!(
            classify_death(&death(None, a.clone(), "RAILGUN", 1)).1,
            Some(IgnoreReason::Unclassified)
        );
        // bot detected by STEAM_ID "0" without BOT flag
        let mut bot2 = bot.clone();
        bot2.bot = None;
        assert_eq!(
            c(&death(Some(a.clone()), bot2, "RAILGUN", 1)),
            Category::Ignored
        );
    }

    fn stats(id: &str, score: i64, win: Option<i64>, quit: bool) -> PlayerStats {
        PlayerStats {
            match_guid: Some("g".into()),
            name: Some(format!("n{id}")),
            steam_id: Some(SteamId(id.into())),
            warmup: Some(false),
            quit: Some(quit),
            score: Some(score),
            win,
            ..Default::default()
        }
    }

    #[test]
    fn duel_by_win() {
        let a = stats("1", 5, Some(0), false);
        let b = stats("2", 3, Some(1), true);
        let w = winners("DUEL", &[&a, &b]);
        assert_eq!(w, BTreeSet::from(["2".to_string()]));
        let a = stats("1", 5, None, false);
        let b = stats("2", 3, None, false);
        assert_eq!(
            winners("DUEL", &[&a, &b]),
            BTreeSet::from(["1".to_string()])
        );
    }

    #[test]
    fn ffa_tie_and_quit_excluded() {
        let a = stats("1", 10, None, false);
        let b = stats("2", 10, None, false);
        let c = stats("3", 20, None, true);
        let bot = stats("0", 30, None, false);
        let w = winners("FFA", &[&a, &b, &c, &bot]);
        assert_eq!(w, BTreeSet::from(["1".to_string(), "2".to_string()]));
    }

    #[test]
    fn accuracy_sums_and_skips_zero_shots() {
        let rows = vec![
            ("RAILGUN", 10, 5),
            ("RAILGUN", 10, 0),
            ("GAUNTLET", 0, 0),
            ("ROCKET", 4, 2),
        ];
        let acc = average_accuracy(rows).unwrap();
        assert!((acc - (0.25 + 0.5) / 2.0).abs() < 1e-9);
        assert_eq!(average_accuracy(vec![("GAUNTLET", 0, 0)]), None);
    }

    #[test]
    fn counted_criteria() {
        assert_eq!(skip_reason(true, true, "FFA", 3), Some(SkipReason::Aborted));
        assert_eq!(
            skip_reason(false, false, "FFA", 3),
            Some(SkipReason::NoStart)
        );
        assert_eq!(
            skip_reason(false, true, "CA", 3),
            Some(SkipReason::TeamMode)
        );
        assert_eq!(skip_reason(false, true, "RACE", 3), Some(SkipReason::Race));
        assert_eq!(
            skip_reason(false, true, "FFA", 1),
            Some(SkipReason::Lt2Humans)
        );
        assert_eq!(skip_reason(false, true, "duel", 2), None);
    }

    #[test]
    fn day_key_boundary() {
        let tz: Tz = "Europe/Moscow".parse().unwrap();
        let a = DateTime::parse_from_rfc3339("2026-10-18T20:59:59.999Z").unwrap();
        let b = DateTime::parse_from_rfc3339("2026-10-18T21:00:00.000Z").unwrap();
        assert_eq!(day_key(a.timestamp_millis(), tz), "2026-10-18");
        assert_eq!(day_key(b.timestamp_millis(), tz), "2026-10-19");
    }

    fn input(deaths: Vec<PlayerDeath>, st: Vec<PlayerStats>) -> MatchInput {
        MatchInput {
            guid: "g".into(),
            started: Some((0, MatchStarted::default())),
            report: MatchReport {
                game_type: Some("FFA".into()),
                aborted: Some(false),
                ..Default::default()
            },
            finished_at: 1000,
            stats: st,
            deaths: deaths
                .into_iter()
                .enumerate()
                .map(|(i, ev)| DeathInput {
                    raw_id: i as i64 + 1,
                    ev,
                })
                .collect(),
            incomplete: false,
        }
    }

    #[test]
    fn streaks() {
        let a = p("1", "A");
        let b = p("2", "B");
        let bot = p("0", "Bot");
        let d = vec![
            death(Some(a.clone()), b.clone(), "RAILGUN", 1),
            death(Some(a.clone()), b.clone(), "RAILGUN", 2),
            death(Some(a.clone()), bot.clone(), "RAILGUN", 3), // бот: не растит стрик
            death(None, a.clone(), "LAVA", 4),                 // обнуляет
            death(Some(a.clone()), b.clone(), "RAILGUN", 5),
            death(Some(a.clone()), a.clone(), "SWITCHTEAM", 6), // не обнуляет
            death(Some(a.clone()), b.clone(), "RAILGUN", 7),
            death(Some(bot.clone()), a.clone(), "RAILGUN", 8), // смерть от бота обнуляет
            death(Some(a.clone()), b.clone(), "RAILGUN", 9),
        ];
        let out = compute_match(&input(d, vec![]));
        let pa = out.players.iter().find(|x| x.steam_id == "1").unwrap();
        assert_eq!(pa.best_streak, 2);
        assert_eq!(pa.kills, 5);
        assert_eq!(pa.env_deaths, 1);
        let pb = out.players.iter().find(|x| x.steam_id == "2").unwrap();
        assert_eq!(pb.deaths, 5);
        assert!(out.row.counted);
        assert_eq!(out.row.human_players, 2);
    }

    #[test]
    fn mod_to_weapon_mapping() {
        let cases = [
            ("GAUNTLET", "GAUNTLET"),
            ("MACHINEGUN", "MACHINEGUN"),
            ("SHOTGUN", "SHOTGUN"),
            ("GRENADE", "GRENADE"),
            ("GRENADE_SPLASH", "GRENADE"),
            ("ROCKET", "ROCKET"),
            ("rocket_splash", "ROCKET"),
            ("LIGHTNING", "LIGHTNING"),
            ("RAILGUN", "RAILGUN"),
            ("PLASMA", "PLASMA"),
            ("PLASMA_SPLASH", "PLASMA"),
            ("BFG", "BFG"),
            ("BFG_SPLASH", "BFG"),
            ("HMG", "HMG"),
            ("CHAINGUN", "CHAINGUN"),
            ("NAILGUN", "NAILGUN"),
            ("PROXIMITY_MINE", "PROXMINE"),
            ("PROXMINE", "PROXMINE"),
            ("PROXMINE_SPLASH", "PROXMINE"),
        ];
        for (m, w) in cases {
            assert_eq!(weapon_for_mod(m), Some(w), "{m}");
            assert!(WEAPON_ORDER.contains(&w));
        }
        for m in [
            "TELEFRAG",
            "KAMIKAZE",
            "UNKNOWN",
            "LAVA",
            "SUICIDE",
            "SWITCHTEAM",
            "",
        ] {
            assert_eq!(weapon_for_mod(m), None, "{m}");
        }
    }

    #[test]
    fn favorite_weapon_tie_break() {
        assert_eq!(favorite_weapon([]), None);
        assert_eq!(favorite_weapon([("RAILGUN", 0)]), None);
        assert_eq!(favorite_weapon([("OTHER_WEAPON", 5)]), None);
        // Больше убийств побеждает независимо от порядка.
        assert_eq!(
            favorite_weapon([("GAUNTLET", 2), ("RAILGUN", 3)]),
            Some(("RAILGUN", 3))
        );
        // Равенство: раньше в WEAPON_ORDER (ROCKET раньше RAILGUN), порядок входа не важен.
        assert_eq!(
            favorite_weapon([("RAILGUN", 4), ("ROCKET", 4)]),
            Some(("ROCKET", 4))
        );
        assert_eq!(
            favorite_weapon([("PROXMINE", 1), ("NAILGUN", 1), ("MACHINEGUN", 1)]),
            Some(("MACHINEGUN", 1))
        );
        // Суммирование по матчам.
        assert_eq!(
            favorite_weapon([("RAILGUN", 2), ("ROCKET", 3), ("RAILGUN", 2)]),
            Some(("RAILGUN", 4))
        );
    }

    #[test]
    fn weapon_kills_only_from_counted_kills() {
        let a = p("1", "A");
        let b = p("2", "B");
        let bot = p("0", "Bot");
        let d = vec![
            death(Some(a.clone()), b.clone(), "RAILGUN", 1),
            death(Some(a.clone()), b.clone(), "TELEFRAG", 2), // kill без оружия
            death(Some(a.clone()), b.clone(), "ROCKET_SPLASH", 3),
            death(Some(a.clone()), bot.clone(), "RAILGUN", 4), // бот: не kill
            death(Some(a.clone()), a.clone(), "ROCKET_SPLASH", 5), // самоподрыв
            death(None, b.clone(), "RAILGUN", 6),              // без KILLER: ignored
            death(Some(b.clone()), a.clone(), "TELEFRAG", 7),
        ];
        let out = compute_match(&input(d, vec![]));
        let pa = out.players.iter().find(|x| x.steam_id == "1").unwrap();
        assert_eq!(pa.kills, 3);
        assert_eq!(
            pa.weapon_kills_json.as_deref(),
            Some(r#"{"RAILGUN":1,"ROCKET":1}"#)
        );
        let pb = out.players.iter().find(|x| x.steam_id == "2").unwrap();
        assert_eq!(pb.kills, 1);
        assert_eq!(pb.weapon_kills_json, None);
    }

    #[test]
    fn streak_order_by_time_then_raw_id() {
        let a = p("1", "A");
        let b = p("2", "B");
        // По порядку получения: kill, kill, lava; по TIME — lava первым.
        let d = vec![
            death(Some(a.clone()), b.clone(), "RAILGUN", 5),
            death(Some(a.clone()), b.clone(), "RAILGUN", 6),
            death(None, a.clone(), "LAVA", 1),
        ];
        let out = compute_match(&input(d, vec![]));
        let pa = out.players.iter().find(|x| x.steam_id == "1").unwrap();
        assert_eq!(pa.best_streak, 2);
    }

    #[test]
    fn one_human_plus_bot_is_lt2() {
        let a = p("1", "A");
        let bot = p("0", "Bot");
        let d = vec![death(Some(a.clone()), bot.clone(), "RAILGUN", 1)];
        let mut st = stats("1", 1, Some(1), false);
        st.weapons = Some(BTreeMap::from([(
            "RAILGUN".to_string(),
            WeaponStats {
                shots: Some(3),
                hits: Some(1),
            },
        )]));
        let out = compute_match(&input(d, vec![st, stats("0", 0, Some(0), false)]));
        assert!(!out.row.counted);
        assert_eq!(out.row.skip_reason, Some(SkipReason::Lt2Humans));
        assert_eq!(out.players.len(), 1);
        assert_eq!(out.weapons.len(), 1);
    }
}
