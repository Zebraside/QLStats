//! HTTP-тесты роутера: формат JSON, ETag/304, gzip, смена суток.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::Duration;
use common::*;
use http_body_util::BodyExt;
use qlstats::collector::CollectorStats;
use qlstats::live::LiveHub;
use qlstats::store;
use qlstats::web::{self, AppState, Clock, FixedClock};
use serde_json::Value;
use tower::ServiceExt;

struct Env {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
    clock: Arc<FixedClock>,
}

fn setup() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let (w, mut live, last) = replay_into(&db, "synthetic.jsonl");
    drop(w);
    live.set_link(true, last);
    let clock = Arc::new(FixedClock::new(last));
    let hub = Arc::new(LiveHub::new(live, 15, tz()));
    let state = AppState::new(
        store::open(&db).unwrap(),
        tz(),
        clock.clone(),
        hub,
        Arc::new(CollectorStats::default()),
        None,
    )
    .unwrap();
    Env {
        _dir: dir,
        state,
        clock,
    }
}

async fn get(
    env: &Env,
    uri: &str,
    inm: Option<&str>,
    gzip: bool,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut req = Request::builder().uri(uri);
    if let Some(e) = inm {
        req = req.header(header::IF_NONE_MATCH, e);
    }
    if gzip {
        req = req.header(header::ACCEPT_ENCODING, "gzip");
    }
    let resp = web::router(env.state.clone())
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, headers, body)
}

#[tokio::test]
async fn summary_etag_304() {
    let env = setup();
    let (st, h, body) = get(&env, "/api/summary", None, false).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(h[header::CACHE_CONTROL], "no-cache");
    let etag = h[header::ETAG].to_str().unwrap().to_string();
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["day"]["date"], "2026-10-19");
    assert_eq!(v["day"]["matches"], 1);
    assert_eq!(v["all_time"]["matches"], 2);
    assert!(v["generated_at"].as_str().unwrap().ends_with("+03:00"));
    let (st, h, body) = get(&env, "/api/summary", Some(&etag), false).await;
    assert_eq!(st, StatusCode::NOT_MODIFIED);
    assert_eq!(h[header::ETAG].to_str().unwrap(), etag);
    assert!(body.is_empty());
    let (st, _, _) = get(&env, "/api/summary", Some("W/\"other\""), false).await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn players_format_and_gzip() {
    let env = setup();
    let (st, h, body) = get(&env, "/api/players", None, true).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(h[header::CONTENT_ENCODING], "gzip");
    let mut d = flate2::read::GzDecoder::new(&body[..]);
    let mut s = String::new();
    std::io::Read::read_to_string(&mut d, &mut s).unwrap();
    let v: Value = serde_json::from_str(&s).unwrap();
    let players = v["players"].as_array().unwrap();
    assert_eq!(players.len(), 3);
    let keys: Vec<&str> = players[0]
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    assert_eq!(
        keys,
        [
            "steam_id",
            "name",
            "matches",
            "kills",
            "deaths",
            "best_streak",
            "wins",
            "accuracy",
            "suicides",
            "env_deaths",
            "fav_weapon"
        ]
    );
    let etag = h[header::ETAG].to_str().unwrap().to_string();
    let (st, _, _) = get(&env, "/api/players", Some(&etag), true).await;
    assert_eq!(st, StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn live_etag_changes_with_state() {
    let env = setup();
    let (st, h, body) = get(&env, "/api/live", None, false).await;
    assert_eq!(st, StatusCode::OK);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["state"], "warmup");
    assert_eq!(v["score_approximate"], true);
    assert!(v["match"].is_null());
    let etag = h[header::ETAG].to_str().unwrap().to_string();
    let (st, _, _) = get(&env, "/api/live", Some(&etag), false).await;
    assert_eq!(st, StatusCode::NOT_MODIFIED);

    // Новое событие меняет версию и ETag.
    let ev = qlstats::model::parse(
        r#"{"TYPE":"MATCH_STARTED","DATA":{"MATCH_GUID":"g","GAME_TYPE":"FFA","MAP":"dm6","FACTORY_TITLE":"Free For All","FRAG_LIMIT":20,"TIME_LIMIT":10,"PLAYERS":[{"NAME":"Bobby","STEAM_ID":"76561190000000002","TEAM":0}]}}"#,
    )
    .unwrap();
    let now = env.clock.now();
    env.state.live.apply(&ev.event, now);
    env.state.refresh_live(now, false);
    let (st, h2, body) = get(&env, "/api/live", Some(&etag), false).await;
    assert_eq!(st, StatusCode::OK);
    assert_ne!(h2[header::ETAG].to_str().unwrap(), etag);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["state"], "in_progress");
    assert_eq!(v["match"]["map"], "dm6");
    assert_eq!(v["match"]["limits"]["frag"], 20);
    assert_eq!(v["match"]["limits"]["time_min"], 10);
    assert_eq!(v["players"][0]["score"], 0);
}

#[tokio::test]
async fn day_rollover_rebuilds_snapshot() {
    let env = setup();
    let (_, h, _) = get(&env, "/api/summary", None, false).await;
    let etag = h[header::ETAG].to_str().unwrap().to_string();
    env.clock.set(env.clock.now() + Duration::days(1));
    let (st, _, body) = get(&env, "/api/summary", Some(&etag), false).await;
    assert_eq!(st, StatusCode::OK);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["day"]["date"], "2026-10-20");
    assert_eq!(v["day"]["matches"], 0);
    assert_eq!(v["day"]["top_wins"]["value"], 0);
    assert_eq!(v["day"]["top_wins"]["players"].as_array().unwrap().len(), 0);
    assert_eq!(v["all_time"]["matches"], 2);
}

#[tokio::test]
async fn healthz_ok() {
    let env = setup();
    let (st, _, body) = get(&env, "/healthz", None, false).await;
    assert_eq!(st, StatusCode::OK);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["status"], "ok");
    assert_eq!(v["db"], "ok");
    assert_eq!(v["zmq"]["connected"], false);
}

#[tokio::test]
async fn static_and_404() {
    let env = setup();
    let (st, _, _) = get(&env, "/../Cargo.toml", None, false).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _, _) = get(&env, "/no-such-file.txt", None, false).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // index.html есть, если фронтенд уже положен в static/.
    if Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("static/index.html")
        .exists()
    {
        let (st, h, _) = get(&env, "/", None, false).await;
        assert_eq!(st, StatusCode::OK);
        assert!(
            h[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/html")
        );
        let etag = h[header::ETAG].to_str().unwrap().to_string();
        let (st, _, _) = get(&env, "/", Some(&etag), false).await;
        assert_eq!(st, StatusCode::NOT_MODIFIED);
    }
}

use std::path::Path;
