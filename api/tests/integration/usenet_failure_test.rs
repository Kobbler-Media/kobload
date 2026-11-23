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
        ("g-nfo", 422, "unimportable", "not a video"),
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
async fn test_an_unimportable_release_is_remembered_but_an_engine_fault_is_not() {
    let rig = UsenetRig::start().await;
    rig.engine
        .refuse("g-nfo", EngineStatus::UNPROCESSABLE_ENTITY, "unimportable");
    rig.engine
        .refuse("g-fault", EngineStatus::INTERNAL_SERVER_ERROR, "internal");
    rig.tv.indexer.set_search_results(vec![
        rig.nzb("g-nfo", "Engine.Nfo.Show.S01E05.2160p.WEB-DL.x264-NZB"),
        rig.nzb("g-fault", "Engine.Nfo.Show.S01E05.1080p.WEB-DL.x264-NZB"),
    ]);
    let options = rig.options("Engine Nfo Show", "5").await;
    for guid in ["g-nfo", "g-fault"] {
        let option = options
            .iter()
            .find(|o| o.nzb_ref == Some(rig.nzb_ref(guid)))
            .unwrap();
        let (status, body) = rig.resolve(json!({ "nzb_ref": option.nzb_ref })).await;
        assert!(!status.is_success(), "{guid}: {body}");
    }

    let options = rig.options("Engine Nfo Show", "5").await;
    let availability = |guid: &str| {
        options
            .iter()
            .find(|o| o.nzb_ref == Some(rig.nzb_ref(guid)))
            .unwrap()
            .availability
    };
    assert_eq!(
        availability("g-nfo"),
        Some(StreamAvailability::Uncached),
        "{options:?}"
    );
    assert_ne!(
        availability("g-fault"),
        Some(StreamAvailability::Uncached),
        "{options:?}"
    );
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_a_tap_during_a_warm_import_gets_a_stream_import_and_no_second_grab() {
    let rig = UsenetRig::start().await;
    rig.tv.indexer.set_search_results(vec![
        rig.nzb("g-raced", "Engine.Raced.Show.S01E05.1080p.WEB-DL.x264-NZB"),
    ]);
    let options = rig.options("Engine Raced Show", "5").await;
    let nzb_ref = options[0].nzb_ref.clone();
    rig.engine.delay(Duration::from_millis(300));

    let warm = rig.resolve(json!({ "nzb_ref": nzb_ref, "priority": "warm" }));
    let tap = async {
        tokio::time::sleep(Duration::from_millis(60)).await;
        rig.resolve(json!({ "nzb_ref": nzb_ref })).await
    };
    let (_, (status, body)) = tokio::join!(warm, tap);
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        rig.engine
            .imports()
            .iter()
            .any(|i| i.mode == usenet_proto::ImportMode::Stream),
        "a viewer's import runs in stream mode"
    );
    assert_eq!(rig.nzbs.fetches("g-raced"), 1, "the indexer is asked once");
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

async fn report(rig: &UsenetRig, key: &str, file_id: Option<i32>) {
    let resp = rig
        .tv
        .server
        .admin_client()
        .post_json(
            "/api/v1/stream/outcome",
            &json!({ "debrid_provider_id": rig.provider, "release_key": key, "ok": false,
                     "file_id": file_id, "reason": "missing_articles" }),
        )
        .await;
    assert_eq!(resp.status, StatusCode::NO_CONTENT, "{}", resp.text());
}

#[tokio::test]
async fn test_reported_missing_articles_mark_the_release_dead_and_a_re_resolve_keeps_it_so() {
    let rig = UsenetRig::start().await;
    rig.tv.indexer.set_search_results(vec![
        rig.nzb("g-holes", "Engine.Holes.Show.S01E05.1080p.WEB-DL.x264-NZB"),
    ]);
    let options = rig.options("Engine Holes Show", "5").await;
    let resolved = rig.resolve_option(&options[0]).await;
    let key = resolved["release_key"].as_str().unwrap().to_string();

    report(&rig, &key, None).await;
    let dead = |options: &[nats_jobs::types::StreamOption], when: &str| {
        assert_eq!(
            options[0].availability,
            Some(StreamAvailability::Uncached),
            "{when}: {options:?}"
        );
    };
    dead(&rig.options("Engine Holes Show", "5").await, "reported");

    let (status, body) = rig.resolve(json!({ "release_key": key })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    dead(
        &rig.options("Engine Holes Show", "5").await,
        "served from the engine's cache",
    );

    rig.engine.evict(&key);
    let (status, body) = rig.resolve(json!({ "release_key": key })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        rig.nzbs.fetches("g-holes"),
        2,
        "the evicted NZB was grabbed again"
    );
    dead(
        &rig.options("Engine Holes Show", "5").await,
        "imported again",
    );
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_missing_articles_in_one_file_of_a_pack_demote_only_that_file() {
    let rig = UsenetRig::start().await;
    rig.engine.layout(
        "g-pack-holes",
        &[
            ("Engine.Pack.Holes.S01E01.1080p.mkv", 1_000_000_000),
            ("Engine.Pack.Holes.S01E02.1080p.mkv", 1_000_000_000),
        ],
    );
    let nzb_ref = rig
        .list("g-pack-holes", "Engine.Pack.Holes.S01.1080p.WEB-DL-GRP")
        .await;
    let (status, e01) = rig
        .resolve(json!({ "nzb_ref": nzb_ref, "episode": { "season": 1, "episode": 1 } }))
        .await;
    assert_eq!(status, StatusCode::OK, "{e01}");
    let key = e01["release_key"].as_str().unwrap().to_string();

    report(&rig, &key, Some(0)).await;

    let (available, reason): (bool, Option<String>) = sqlx::query_as(
        "SELECT available, reason FROM debrid_availability_cache
         WHERE release_key = $1 AND provider_id = $2",
    )
    .bind(&key)
    .bind(rig.provider)
    .fetch_one(&rig.tv.server.pool)
    .await
    .unwrap();
    assert_eq!((available, reason), (true, None), "the pack is not dead");
    let failed: Vec<Option<i32>> = sqlx::query_scalar(
        "SELECT file_id FROM stream_release_outcomes WHERE release_key = $1 AND failures > 0",
    )
    .bind(&key)
    .fetch_all(&rig.tv.server.pool)
    .await
    .unwrap();
    assert_eq!(failed, vec![Some(0)]);
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_an_engine_restart_that_dropped_a_release_re_imports_it_on_re_resolve() {
    let rig = UsenetRig::start().await;
    let nzb_ref = rig
        .list(
            "g-evicted",
            "Engine.Evicted.Show.S01E05.1080p.WEB-DL.x264-NZB",
        )
        .await;
    let (status, first) = rig.resolve(json!({ "nzb_ref": nzb_ref })).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let key = first["release_key"].as_str().unwrap().to_string();

    rig.engine.evict(&key);
    let before = rig.engine.imports().len();
    let (status, again) = rig
        .resolve(json!({ "release_key": key, "file_id": first["file_id"] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["release_key"], key.as_str());
    let imports = rig.engine.imports();
    let sent_nzb: Vec<bool> = imports[before..]
        .iter()
        .map(|i| i.nzb_base64.is_some())
        .collect();
    assert_eq!(
        sent_nzb,
        [false, true],
        "the engine is asked by cache key first, then sent the NZB"
    );
    assert_eq!(rig.nzbs.fetches("g-evicted"), 2);
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

#[test]
fn test_the_shipped_env_templates_start() {
    for template in ["env.dev.template", ".env.example"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(template);
        let text = std::fs::read_to_string(&path).unwrap();
        let vars: Vec<(String, String)> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();
        let settings = EngineSettings::from_lookup(|name| {
            vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
        });
        assert!(
            matches!(settings, Ok(None)),
            "{template}: {:?}",
            settings.err()
        );
    }
}
