#![allow(dead_code)]

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use qlstats::live::LiveState;
use qlstats::replay::{self, Frame};
use qlstats::store::{self, Writer};

pub fn tz() -> Tz {
    "Europe/Moscow".parse().unwrap()
}

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

pub fn frames(name: &str) -> Vec<Frame> {
    replay::read_jsonl(&fixture(name)).unwrap()
}

/// Прогоняет фикстуру в БД по пути `db`; возвращает writer, live и время последнего события.
pub fn replay_into(db: &Path, name: &str) -> (Writer, LiveState, DateTime<Utc>) {
    let fr = frames(name);
    let conn = store::open(db).unwrap();
    let mut w = Writer::new(conn, tz(), None);
    let mut live = LiveState::new(fr[0].received_at);
    let last = replay::run(&fr, &mut w, &mut live).unwrap();
    (w, live, last)
}
