//! How the usenet stream path fails: the text kobbler classifies, what is remembered so a
//! refused NZB is not grabbed again, and the bookkeeping that keeps listings bounded.

use std::time::{Duration, Instant};

use axum::http::StatusCode as EngineStatus;
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::common::usenet::{CONTROL_KEY, INDEXER_KEY, STREAM_SECRET, Served, UsenetRig};
use kobload_api::jobs::handlers::CacheCleanupHandler;
use kobload_api::jobs::worker::JobHandler;
use kobload_api::repositories::{app_settings, usenet_releases};
use kobload_api::usenet::EngineSettings;
use nats_jobs::types::StreamAvailability;

fn text(body: &Value) -> &str {
    body.as_str().unwrap_or_default()
}

#[tokio::test]
async fn test_engine_refusals_read_as_kobbler_classifies_them() {
    let rig = UsenetRig::start().await;
    app_settings::set(
        &rig.tv.server.pool,
        app_settings::keys::DEBRID_LOGGING_ENABLED,
        &json!(true),
    )
    .await
    .unwrap();
    let cases = [
        ("g-missing", 422, "missing_articles", "not cached"),
        ("g-old", 422, "retention", "not cached"),
        ("g-bad", 400, "invalid_nzb", "not cached"),
        ("g-locked", 422, "password", "blocked"),
        ("g-rar", 422, "unsupported", "not a video"),
        ("g-nested", 422, "nested_nzb", "not a video"),
    ];
    for (guid, status, code, phrase) in cases {
        rig.engine
            .refuse(guid, EngineStatus::from_u16(status).unwrap(), code);
        let nzb_ref = rig
            .list(guid, "Engine.Refused.2026.1080p.WEB-DL.x264-NZB")
            .await;
        let (status, body) = rig.resolve(json!({ "nzb_ref": nzb_ref })).await;
        assert!(!status.is_success(), "{guid}: {body}");
        let message = text(&body).to_lowercase();
        assert!(message.contains(phrase), "{guid}: {message}");
        assert!(message.contains(code), "{guid}: {message}");
        for other in ["not cached", "blocked", "not a video"]
            .iter()
            .filter(|p| **p != phrase)
        {
            assert!(
                !message.contains(other),
                "{guid} reads as {other}: {message}"
            );
        }
    }

    let attempts: Vec<Value> = rig
        .tv
        .server
        .admin_client()
        .get("/api/v1/stream-attempts")
        .await
        .json();
    assert_eq!(attempts.len(), cases.len(), "{attempts:?}");
    for attempt in &attempts {
        let key = attempt["release_key"].as_str().unwrap();
        assert!(
            key.len() == 64 && key.chars().all(|c| c.is_ascii_hexdigit()),
            "a failed attempt is keyed by the opaque ref, not indexer/guid: {key}"
        );
    }

    rig.engine
        .layout("g-docs", &[("Engine.Docs.2026.nfo", 9_000)]);
    let nzb_ref = rig
        .list("g-docs", "Engine.Docs.2026.1080p.WEB-DL.x264-NZB")
        .await;
    let (status, body) = rig.resolve(json!({ "nzb_ref": nzb_ref })).await;
    assert!(!status.is_success(), "{body}");
    assert!(text(&body).contains("is not a video"), "{body}");
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_an_indexer_refusal_is_remembered_so_taps_do_not_grab_again() {
    let rig = UsenetRig::start().await;
    rig.nzbs
        .serve("g-gone", Served::Status(StatusCode::NOT_FOUND));
    rig.nzbs
        .serve("g-busy", Served::Status(StatusCode::TOO_MANY_REQUESTS));
    rig.engine
        .refuse("g-junk", EngineStatus::BAD_REQUEST, "invalid_nzb");
    for (guid, phrase) in [
        ("g-gone", "not cached"),
        ("g-busy", "rate limit"),
        ("g-junk", "not cached"),
    ] {
        let nzb_ref = rig
            .list(guid, "Engine.Gone.2026.1080p.WEB-DL.x264-NZB")
            .await;
        let (_, first) = rig.resolve(json!({ "nzb_ref": nzb_ref })).await;
        let (status, again) = rig.resolve(json!({ "nzb_ref": nzb_ref })).await;
        assert!(!status.is_success(), "{guid}: {again}");
        assert_eq!(first, again, "{guid}");
        assert!(text(&again).contains(phrase), "{guid}: {again}");
        assert!(!text(&again).contains(INDEXER_KEY), "{again}");
        assert_eq!(rig.nzbs.fetches(guid), 1, "{guid} was grabbed again");
    }
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_an_nzb_over_the_cap_is_refused_without_reading_it_all() {
    let rig = UsenetRig::start().await;
    rig.nzbs.serve("g-huge", Served::Huge);
    let nzb_ref = rig
        .list("g-huge", "Engine.Huge.2026.1080p.WEB-DL.x264-NZB")
        .await;
    let (status, body) = rig.resolve(json!({ "nzb_ref": nzb_ref })).await;
    assert!(!status.is_success(), "{body}");
    assert!(text(&body).contains("over 128 MB"), "{body}");
    assert_eq!(rig.engine.imports_with_nzb(), 0);
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_a_caller_that_hangs_up_leaves_no_flight_for_the_next_tap_to_join() {
    let rig = UsenetRig::start().await;
    rig.engine.delay(Duration::from_millis(400));
    rig.engine.refuse(
        "g-flaky",
        EngineStatus::SERVICE_UNAVAILABLE,
        "providers_unavailable",
    );
    let nzb_ref = rig
        .list("g-flaky", "Engine.Flaky.2026.1080p.WEB-DL.x264-NZB")
        .await;
    let body = json!({ "nzb_ref": nzb_ref, "debrid_provider_id": rig.provider });

    let impatient = reqwest::Client::builder()
        .timeout(Duration::from_millis(100))
        .build()
        .unwrap()
        .post(format!("{}/api/v1/stream/resolve", rig.tv.server.api_url))
        .bearer_auth(&rig.tv.server.admin_token)
        .json(&body)
        .send()
        .await;
    assert!(impatient.is_err(), "the first caller hung up");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    rig.engine.accept("g-flaky");
    rig.engine.delay(Duration::ZERO);

    let (status, resolved) = rig.resolve(json!({ "nzb_ref": nzb_ref })).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the next tap imported afresh instead of joining the finished flight: {resolved}"
    );
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_a_search_asks_the_engine_for_every_imported_pack_at_once() {
    let rig = UsenetRig::start().await;
    let guids = ["g-l1", "g-l2", "g-l3"];
    rig.tv.indexer.set_search_results(
        guids
            .iter()
            .zip(["480p.HDTV", "720p.WEB-DL", "1080p.WEB-DL"])
            .map(|(g, quality)| {
                rig.nzb(g, &format!("Engine.Lookup.Show.S01E05.{quality}.x264-NZB"))
            })
            .collect(),
    );
    rig.options("Engine Lookup Show", "5").await;
    for guid in guids {
        let key = format!("nzb-{}", Uuid::new_v4().simple());
        rig.engine
            .hold(&key, &[("Engine.Lookup.Show.S01E05.mkv", 1_000)]);
        usenet_releases::set_usenet_release_key(&rig.tv.server.pool, &rig.nzb_ref(guid), &key)
            .await
            .unwrap();
    }
    rig.engine.release_delay(Duration::from_millis(1200));

    let started = Instant::now();
    let options = rig.options("Engine Lookup Show", "5").await;
    let took = started.elapsed();
    assert_eq!(options.len(), 3, "{options:?}");
    assert!(
        options.iter().all(|o| o.files.len() == 1),
        "every lookup answered within the one deadline: {options:?}"
    );
    assert!(
        took < Duration::from_millis(2500),
        "lookups ran one after another: {took:?}"
    );
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_reported_missing_articles_mark_the_release_dead() {
    let rig = UsenetRig::start().await;
    rig.tv.indexer.set_search_results(vec![
        rig.nzb("g-holes", "Engine.Holes.Show.S01E05.1080p.WEB-DL.x264-NZB"),
    ]);
    let options = rig.options("Engine Holes Show", "5").await;
    let resolved = rig.resolve_option(&options[0]).await;
    let key = resolved["release_key"].as_str().unwrap();

    let resp = rig
        .tv
        .server
        .admin_client()
        .post_json(
            "/api/v1/stream/outcome",
            &json!({ "debrid_provider_id": rig.provider, "release_key": key, "ok": false,
                     "reason": "missing_articles" }),
        )
        .await;
    assert_eq!(resp.status, StatusCode::NO_CONTENT, "{}", resp.text());

    let options = rig.options("Engine Holes Show", "5").await;
    assert_eq!(
        options[0].availability,
        Some(StreamAvailability::Uncached),
        "{options:?}"
    );
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_cleanup_prunes_old_unimported_listings_and_backups_drop_indexer_urls() {
    let rig = UsenetRig::start().await;
    let pool = &rig.tv.server.pool;
    rig.list("g-stale", "Engine.Stale.2026.1080p.WEB-DL.x264-NZB")
        .await;
    rig.list("g-fresh", "Engine.Fresh.2026.1080p.WEB-DL.x264-NZB")
        .await;
    let kept = rig
        .list("g-kept", "Engine.Kept.2026.1080p.WEB-DL.x264-NZB")
        .await;
    usenet_releases::set_usenet_release_key(pool, &kept, "nzb-kept")
        .await
        .unwrap();
    sqlx::query(
        "UPDATE usenet_releases SET updated_at = NOW() - INTERVAL '31 days' WHERE guid <> 'g-fresh'",
    )
    .execute(pool)
    .await
    .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("backup.tar.gz");
    kobload_api::backup::create_backup(pool, &archive)
        .await
        .unwrap();
    let mut csv = String::new();
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(
        std::fs::File::open(&archive).unwrap(),
    ));
    for entry in tar.entries().unwrap() {
        let mut entry = entry.unwrap();
        if entry.path().unwrap().ends_with("usenet_releases.csv") {
            std::io::Read::read_to_string(&mut entry, &mut csv).unwrap();
        }
    }
    assert!(csv.contains("g-stale"), "{csv}");
    assert!(!csv.contains(INDEXER_KEY), "{csv}");

    CacheCleanupHandler::new(pool.clone())
        .execute(
            Uuid::new_v4(),
            json!({}),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    let mut left: Vec<String> = sqlx::query_scalar("SELECT guid FROM usenet_releases")
        .fetch_all(pool)
        .await
        .unwrap();
    left.sort();
    assert_eq!(left, ["g-fresh", "g-kept"]);
    rig.tv.server.cleanup().await;
}

#[test]
fn test_weak_or_partial_engine_settings_refuse_to_start() {
    let settings = |vars: &[(&str, &str)]| {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        EngineSettings::from_lookup(move |name| {
            vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
        })
    };
    let full = |control: &str, secret: &str| {
        settings(&[
            ("USENET_ENGINE_URL", "http://usenet:7090"),
            ("USENET_STREAM_BASE_URL", "http://10.0.0.145:7090"),
            ("USENET_CONTROL_KEY", control),
            ("USENET_STREAM_SECRET", secret),
        ])
    };
    assert!(matches!(settings(&[]), Ok(None)));
    assert!(matches!(full(CONTROL_KEY, STREAM_SECRET), Ok(Some(_))));
    assert!(full("dev-usenet-control-key", STREAM_SECRET).is_err());
    assert!(full(CONTROL_KEY, "short").is_err());
    assert!(full(CONTROL_KEY, CONTROL_KEY).is_err());
    assert!(settings(&[("USENET_CONTROL_KEY", CONTROL_KEY)]).is_err());
}
