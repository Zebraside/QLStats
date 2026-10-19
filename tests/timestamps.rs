//! Таймкоды: у каждого сохранённого события есть `received_at` в мс UTC, индексы на месте.

mod common;

use common::*;
use rusqlite::Connection;

/// Порог «это миллисекунды»: 10^12 мс = 2001-09-09; секунды сейчас порядка 1,8·10^9.
const MS_MIN: i64 = 1_000_000_000_000;

fn has_index(conn: &Connection, table: &str, name: &str) -> bool {
    let mut st = conn
        .prepare(&format!("PRAGMA index_list({table})"))
        .unwrap();
    let names: Vec<String> = st
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    names.iter().any(|n| n == name)
}

#[test]
fn raw_events_have_millisecond_timestamps() {
    let dir = tempfile::tempdir().unwrap();
    let (w, _live, _last) = replay_into(&dir.path().join("t.db"), "synthetic.jsonl");
    let conn = w.conn();

    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM raw_events", [], |r| r.get(0))
        .unwrap();
    assert!(total > 0);
    let bad: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM raw_events WHERE received_at IS NULL OR received_at < ?1",
            [MS_MIN],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        bad, 0,
        "received_at должен быть > 10^12 (мс), не NULL/0/секунды"
    );

    let mut st = conn
        .prepare("SELECT received_at FROM raw_events ORDER BY id")
        .unwrap();
    let ts: Vec<i64> = st
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(
        ts.windows(2).all(|p| p[0] <= p[1]),
        "received_at убывает по id"
    );
}

#[test]
fn matches_have_consistent_times() {
    let dir = tempfile::tempdir().unwrap();
    let (w, _live, _last) = replay_into(&dir.path().join("t.db"), "synthetic.jsonl");
    let ms = store_matches(w.conn());
    assert!(ms.iter().any(|m| m.2), "в фикстуре нет засчитанных матчей");
    for (guid, started, counted, finished) in ms.iter().map(|m| (&m.0, m.1, m.2, m.3)) {
        assert!(finished >= MS_MIN, "{guid}: finished_at не в мс");
        if counted {
            let s = started.unwrap_or_else(|| panic!("{guid}: засчитан без started_at"));
            assert!(
                s >= MS_MIN && s <= finished,
                "{guid}: started_at {s} > finished_at {finished}"
            );
        } else if let Some(s) = started {
            assert!(
                s >= MS_MIN && s <= finished,
                "{guid}: started_at вне порядка"
            );
        }
    }
}

fn store_matches(conn: &Connection) -> Vec<(String, Option<i64>, bool, i64)> {
    let mut st = conn
        .prepare("SELECT guid, started_at, counted, finished_at FROM matches")
        .unwrap();
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

#[test]
fn time_indexes_exist() {
    let dir = tempfile::tempdir().unwrap();
    let (w, _live, _last) = replay_into(&dir.path().join("t.db"), "synthetic.jsonl");
    assert!(has_index(w.conn(), "raw_events", "raw_events_received_at"));
    assert!(has_index(w.conn(), "matches", "matches_finished_at"));
}

#[test]
fn recompute_keeps_match_times() {
    let dir = tempfile::tempdir().unwrap();
    let (w, _live, _last) = replay_into(&dir.path().join("t.db"), "synthetic.jsonl");
    let before = store_matches(w.conn());
    drop(w);
    let mut conn = qlstats::store::open(&dir.path().join("t.db")).unwrap();
    qlstats::store::recompute(&mut conn, tz()).unwrap();
    let mut after = store_matches(&conn);
    let mut before = before;
    before.sort();
    after.sort();
    assert_eq!(before, after);
}

#[test]
fn migration_v3_adds_time_indexes_to_v2_db() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    {
        let (w, _, _) = replay_into(&db, "synthetic.jsonl");
        w.conn()
            .execute_batch(
                "DROP INDEX raw_events_received_at; DROP INDEX matches_finished_at;
                 PRAGMA user_version = 2;",
            )
            .unwrap();
    }
    let conn = qlstats::store::open(&db).unwrap();
    let ver: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(ver, 3);
    assert!(has_index(&conn, "raw_events", "raw_events_received_at"));
    assert!(has_index(&conn, "matches", "matches_finished_at"));
}
