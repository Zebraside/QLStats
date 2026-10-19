//! Интеграционные тесты replay: golden JSON, recompute, повторный прогон.

mod common;

use std::path::Path;

use common::*;
use qlstats::replay;
use qlstats::store;
use serde_json::Value;

fn golden(name: &str, actual: &Value) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.json"));
    let text = serde_json::to_string_pretty(actual).unwrap() + "\n";
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, &text).unwrap();
    }
    let expected: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        &expected, actual,
        "golden {name} не совпал; UPDATE_GOLDEN=1 перезапишет эталон"
    );
}

fn dump_of(name: &str) -> (Value, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let (w, live, last) = replay_into(&dir.path().join("t.db"), name);
    let v = replay::dump(w.conn(), &live, last, tz(), 15).unwrap();
    (v, dir)
}

#[test]
fn golden_synthetic() {
    let (v, _d) = dump_of("synthetic.jsonl");
    golden("synthetic", &v);
}

#[test]
fn golden_sample_long() {
    let (v, _d) = dump_of("sample-long.jsonl");
    // Реальный конец FFA-матча: 1 человек + бот.
    let matches = v["matches"].as_array().unwrap();
    assert!(
        matches
            .iter()
            .any(|m| m["counted"] == false && m["skip_reason"] == "lt2_humans")
    );
    assert!(matches.iter().all(|m| m["counted"] == false));
    assert_eq!(v["live"]["state"], "empty");
    golden("sample-long", &v);
}

#[test]
fn golden_sample_60s() {
    let (v, _d) = dump_of("sample-60s.jsonl");
    // Матч не завершился: в matches пусто, live — идёт матч не виден (нет MATCH_STARTED),
    // но человек на сервере есть.
    assert_eq!(v["matches"].as_array().unwrap().len(), 0);
    assert_eq!(v["live"]["state"], "warmup");
    golden("sample-60s", &v);
}

fn stats_part(v: &Value) -> Value {
    serde_json::json!({"summary": v["summary"], "players": v["players"], "matches": v["matches"]})
}

#[test]
fn recompute_gives_same_result() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let (w, live, last) = replay_into(&db, "synthetic.jsonl");
    let before = replay::dump(w.conn(), &live, last, tz(), 15).unwrap();
    drop(w);
    let mut conn = store::open(&db).unwrap();
    let backup = dir.path().join("backup.db");
    store::backup(&conn, &backup).unwrap();
    assert!(backup.exists());
    let sum = store::recompute(&mut conn, tz()).unwrap();
    assert_eq!(sum.matches, 5);
    assert_eq!(sum.counted, 2);
    let after = replay::dump(&conn, &live, last, tz(), 15).unwrap();
    assert_eq!(stats_part(&before), stats_part(&after));
    // Таблица deaths пересоздана без дублей.
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM deaths", [], |r| r.get(0))
        .unwrap();
    let n_before: i64 = {
        let dir2 = tempfile::tempdir().unwrap();
        let (w2, _, _) = replay_into(&dir2.path().join("x.db"), "synthetic.jsonl");
        w2.conn()
            .query_row("SELECT COUNT(*) FROM deaths", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(n, n_before);
}

#[test]
fn double_replay_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let (w, live, last) = replay_into(&db, "synthetic.jsonl");
    let once = replay::dump(w.conn(), &live, last, tz(), 15).unwrap();
    drop(w);
    let (w, live2, last2) = replay_into(&db, "synthetic.jsonl");
    let twice = replay::dump(w.conn(), &live2, last2, tz(), 15).unwrap();
    assert_eq!(stats_part(&once), stats_part(&twice));
    let raw: i64 = w
        .conn()
        .query_row("SELECT COUNT(*) FROM raw_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(raw as usize, 2 * frames("synthetic.jsonl").len());
}

#[test]
fn invalid_frames_are_stored_raw() {
    let dir = tempfile::tempdir().unwrap();
    let (w, _, _) = replay_into(&dir.path().join("t.db"), "synthetic.jsonl");
    let (ty, payload): (String, String) = w
        .conn()
        .query_row(
            "SELECT type, payload FROM raw_events WHERE type = '_INVALID'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(ty, "_INVALID");
    assert_eq!(payload, "this is not json");
}

#[test]
fn incomplete_flag_on_link_gap() {
    use qlstats::store::{Writer, WriterMsg};
    let dir = tempfile::tempdir().unwrap();
    let conn = store::open(&dir.path().join("t.db")).unwrap();
    let mut w = Writer::new(conn, tz(), None);
    let fr = frames("synthetic.jsonl");
    // Обрыв ZMQ посреди матча 1 (после первых смертей).
    for (i, f) in fr.iter().enumerate() {
        if i == 20 {
            w.handle(WriterMsg::Link {
                connected: false,
                at: f.received_at.timestamp_millis(),
            });
            w.handle(WriterMsg::Link {
                connected: true,
                at: f.received_at.timestamp_millis(),
            });
        }
        w.process_frame(f.received_at.timestamp_millis(), &f.bytes)
            .unwrap();
    }
    let ms = store::list_matches(w.conn()).unwrap();
    let m1 = ms.iter().find(|m| m.guid.starts_with("1111")).unwrap();
    assert!(m1.incomplete);
    assert!(m1.counted);
    let m5 = ms.iter().find(|m| m.guid.starts_with("5555")).unwrap();
    assert!(!m5.incomplete);
}

#[test]
fn migration_adds_weapon_kills_to_v1_db() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    // БД со схемой v1: без match_players.weapon_kills, user_version = 1.
    {
        let (w, _, _) = replay_into(&db, "synthetic.jsonl");
        w.conn()
            .execute_batch(
                "ALTER TABLE match_players DROP COLUMN weapon_kills; PRAGMA user_version = 1;",
            )
            .unwrap();
    }
    let mut conn = store::open(&db).unwrap();
    let ver: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(ver, 2);
    // Старые матчи без weapon_kills: таблица строится, fav_weapon пуст.
    let before = store::player_totals(&conn).unwrap();
    assert!(!before.is_empty());
    assert!(before.iter().all(|p| p.fav_weapon.is_none()));
    // После recompute любимое оружие появляется.
    store::recompute(&mut conn, tz()).unwrap();
    let after = store::player_totals(&conn).unwrap();
    let alice = after
        .iter()
        .find(|p| p.steam_id == "76561190000000001")
        .unwrap();
    let fav = alice.fav_weapon.as_ref().unwrap();
    assert_eq!((fav.weapon, fav.kills), ("RAILGUN", 5));
}

#[test]
fn pretty_json_is_stored_compact() {
    use qlstats::store::Writer;
    let dir = tempfile::tempdir().unwrap();
    let conn = store::open(&dir.path().join("t.db")).unwrap();
    let mut w = Writer::new(conn, tz(), None);
    // Так присылает игровой сервер: отступы в три пробела и пробел перед двоеточием.
    let pretty = "{\n   \"TYPE\" : \"PLAYER_CONNECT\",\n   \"DATA\" : {\n      \"NAME\" : \"Zed\",\n      \"STEAM_ID\" : \"76561190000000009\",\n      \"MATCH_GUID\" : \"g-1\",\n      \"TIME\" : 1\n   }\n}\n";
    w.process_frame(1_700_000_000_000, pretty.as_bytes())
        .unwrap();
    let (guid, payload): (String, String) = w
        .conn()
        .query_row(
            "SELECT match_guid, payload FROM raw_events WHERE type = 'PLAYER_CONNECT'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(guid, "g-1");
    assert!(
        !payload.contains('\n') && !payload.contains("\" :"),
        "{payload}"
    );
    assert!(payload.len() < pretty.len() * 3 / 4, "{payload}");
    // Содержимое и порядок ключей не изменились.
    let a: Value = serde_json::from_str(pretty).unwrap();
    let b: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(a, b);
    assert!(payload.starts_with("{\"TYPE\":\"PLAYER_CONNECT\",\"DATA\":{\"NAME\""));
}
