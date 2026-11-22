//! Usenet streaming through our own engine: kobload dereferences an `nzb_ref`, imports the
//! NZB into the engine (fetching it from the indexer only when the engine lacks it), picks
//! the output the caller wants and hands back signed engine URLs.

use std::time::Duration;

use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::common::usenet::{UsenetRig, assert_purpose, claims, purposes};
use kobload_api::repositories::{app_settings, usenet_releases};
use nats_jobs::types::{StreamAvailability, StreamPlayback};
use usenet_proto::Purpose;

const GB: u64 = 1_000_000_000;

fn pack_layout(rig: &UsenetRig, guid: &str, show: &str) {
    rig.engine.layout(
        guid,
        &[
            (&format!("{show}.S01E01.1080p.mkv"), GB),
            (&format!("{show}.S01E02E03.1080p.mkv"), 2 * GB),
            (&format!("{show}.S01E04.1080p.en.srt"), 50_000),
            (&format!("{show}.S01E04.1080p.mkv"), GB),
            (&format!("Sample/{show}.S01E04.sample.mkv"), 50_000_000),
        ],
    );
}

#[tokio::test]
async fn test_search_lists_usenet_options_on_the_usenet_provider() {
    let rig = UsenetRig::start().await;
    rig.tv.indexer.set_search_results(vec![
        rig.nzb("g-list", "Engine.List.Show.S01E05.1080p.WEB-DL.x264-NZB"),
        crate::common::tv_watch::torrent_release(
            "t-list",
            "Engine.List.Show.S01E05.720p.WEB-DL.x264-TOR",
            'e',
        ),
    ]);
    let all = rig
        .tv
        .stream_options(&crate::common::tv_watch::episode_want(
            "Engine List Show",
            "1",
            "5",
            None,
        ))
        .await;
    assert_eq!(
        all.len(),
        1,
        "a usenet-only provider offers no torrents: {all:?}"
    );
    let option = &all[0];
    assert_eq!(option.debrid_provider, "usenet");
    assert_eq!(option.debrid_provider_id, rig.provider);
    assert_eq!(option.nzb_ref, Some(rig.nzb_ref("g-list")));
    assert!(!option.id.contains("g-list"), "{}", option.id);
    assert_eq!(
        option.availability,
        Some(StreamAvailability::Likely),
        "a fresh usenet pick on our engine is likely, so kobbler may warm it"
    );
    assert!(rig.engine.imports().is_empty(), "searching imports nothing");
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_resolve_imports_once_and_returns_signed_engine_urls() {
    let rig = UsenetRig::start().await;
    app_settings::set(
        &rig.tv.server.pool,
        app_settings::keys::DEBRID_LOGGING_ENABLED,
        &json!(true),
    )
    .await
    .unwrap();
    rig.engine.layout(
        "g-movie",
        &[
            ("Engine.Movie.2026.1080p.nfo", 9_000),
            ("Engine.Movie.2026.1080p.mkv", 4 * GB),
            ("Engine.Movie.2026.1080p.sample.mkv", 40_000_000),
            ("Extras/Engine.Movie.2026.Featurette.mkv", GB),
        ],
    );
    let nzb_ref = rig
        .list("g-movie", "Engine.Movie.2026.1080p.BluRay.x264-NZB")
        .await;

    let (status, first) = rig.resolve(json!({ "nzb_ref": nzb_ref })).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let release_key = first["release_key"].as_str().unwrap().to_string();
    assert!(release_key.starts_with("nzb-"), "{first}");
    assert_eq!(first["cached"], true);
    assert_eq!(first["filename"], "Engine.Movie.2026.1080p.mkv");
    assert_eq!(first["filesize"], 4 * GB);
    assert_eq!(first["file_id"], 1);
    assert!(first["torrent_id"].is_null(), "{first}");
    let (play, cache) = purposes(&first);
    assert_purpose(&play, Purpose::Play);
    assert_purpose(&cache, Purpose::Cache);
    for c in [&play, &cache] {
        assert_eq!(c.release_key, release_key);
        assert_eq!(c.output, 1);
        let ttl = c.expires_at - time::OffsetDateTime::now_utc().unix_timestamp();
        assert!((6 * 3600 - 60..=6 * 3600).contains(&ttl), "ttl {ttl}");
    }
    let expires_at = time::OffsetDateTime::parse(
        first["expires_at"].as_str().unwrap(),
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    assert_eq!(expires_at.unix_timestamp(), play.expires_at);
    assert!(
        !first
            .to_string()
            .contains(crate::common::usenet::INDEXER_KEY),
        "{first}"
    );

    let imports = rig.engine.imports();
    assert_eq!(imports.len(), 2, "{imports:?}");
    assert!(
        imports
            .iter()
            .all(|i| i.cache_key == rig.cache_key("g-movie"))
    );
    assert!(imports[0].nzb_base64.is_none(), "the engine is asked first");
    assert!(imports[1].nzb_base64.is_some());
    assert_eq!(rig.nzbs.fetches("g-movie"), 1);

    let (status, second) = rig.resolve(json!({ "nzb_ref": nzb_ref })).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["release_key"], release_key.as_str());
    assert_eq!(rig.nzbs.fetches("g-movie"), 1, "the engine had it cached");
    let imports = rig.engine.imports();
    assert_eq!(imports.len(), 3);
    assert!(imports[2].nzb_base64.is_none());
    assert_eq!(imports[2].cache_key, rig.cache_key("g-movie"));

    let pool = &rig.tv.server.pool;
    let stored = usenet_releases::get(pool, &nzb_ref).await.unwrap().unwrap();
    assert_eq!(stored.release_key.as_deref(), Some(release_key.as_str()));
    let available: Option<bool> = sqlx::query_scalar(
        "SELECT available FROM debrid_availability_cache WHERE release_key = $1 AND provider_id = $2",
    )
    .bind(&release_key)
    .bind(rig.provider)
    .fetch_optional(pool)
    .await
    .unwrap();
    assert_eq!(available, Some(true));

    let attempts: Vec<Value> = rig
        .tv
        .server
        .admin_client()
        .get("/api/v1/stream-attempts")
        .await
        .json();
    assert_eq!(attempts.len(), 2, "{attempts:?}");
    for attempt in &attempts {
        assert_eq!(attempt["provider_id"], rig.provider.to_string());
        assert_eq!(attempt["release_key"], release_key.as_str());
        assert_eq!(attempt["status"], "ready", "{attempt}");
    }
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_concurrent_resolves_of_one_release_import_once() {
    let rig = UsenetRig::start().await;
    rig.engine.delay(Duration::from_millis(300));
    rig.tv.indexer.set_search_results(vec![
        rig.nzb("g-race", "Engine.Race.Show.S01E05.1080p.WEB-DL.x264-NZB"),
    ]);
    let options = rig.options("Engine Race Show", "5").await;
    let nzb_ref = options[0].nzb_ref.clone().unwrap();

    let resolves = (0..5).map(|_| rig.resolve(json!({ "nzb_ref": nzb_ref })));
    let results = futures::future::join_all(resolves).await;
    let keys: Vec<String> = results
        .iter()
        .map(|(status, body)| {
            assert_eq!(*status, StatusCode::OK, "{body}");
            body["release_key"].as_str().unwrap().to_string()
        })
        .collect();
    assert!(keys.windows(2).all(|w| w[0] == w[1]), "{keys:?}");
    assert_eq!(rig.nzbs.fetches("g-race"), 1);
    assert_eq!(rig.engine.imports_with_nzb(), 1);
    assert_eq!(rig.engine.imports().len(), 2, "one flight: ask, then send");
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_an_episode_resolves_to_its_output_and_a_multi_episode_file_serves_each() {
    let rig = UsenetRig::start().await;
    pack_layout(&rig, "g-pack", "Engine.Pack.Show");
    rig.tv.indexer.set_search_results(vec![
        rig.nzb("g-pack", "Engine.Pack.Show.S01E02E03.1080p.WEB-DL.x264-NZB"),
    ]);

    let options = rig.options("Engine Pack Show", "3").await;
    assert_eq!(options.len(), 1, "{options:?}");
    assert!(
        options[0].files.is_empty(),
        "nothing is known before import"
    );
    let resolved = rig.resolve_option(&options[0]).await;
    assert_eq!(claims(resolved["stream_url"].as_str().unwrap()).output, 1);
    assert_eq!(resolved["filename"], "Engine.Pack.Show.S01E02E03.1080p.mkv");
    let key = resolved["release_key"].as_str().unwrap().to_string();

    let options = rig.options("Engine Pack Show", "2").await;
    let option = &options[0];
    assert_eq!(option.release_key.as_deref(), Some(key.as_str()));
    assert_eq!(option.availability, Some(StreamAvailability::Ready));
    let ids: Vec<i32> = option.files.iter().map(|f| f.id).collect();
    assert_eq!(
        ids[0], 1,
        "the wanted episode is listed first: {:?}",
        option.files
    );
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, vec![0, 1, 2, 3, 4], "every output is listed");
    let resolved = rig.resolve_option(option).await;
    assert_eq!(claims(resolved["stream_url"].as_str().unwrap()).output, 1);

    let (status, next) = rig
        .resolve(json!({ "release_key": key, "release_title": "Engine.Pack.Show.S01E04.1080p" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{next}");
    assert_eq!(next["filename"], "Engine.Pack.Show.S01E04.1080p.mkv");

    let (status, by_id) = rig
        .resolve(json!({ "release_key": key, "file_id": 0 }))
        .await;
    assert_eq!(status, StatusCode::OK, "{by_id}");
    assert_eq!(claims(by_id["stream_url"].as_str().unwrap()).output, 0);
    assert_eq!(by_id["filename"], "Engine.Pack.Show.S01E01.1080p.mkv");
    let (status, by_name) = rig
        .resolve(json!({ "release_key": key, "file_name": "Engine.Pack.Show.S01E02E03.1080p.mkv" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{by_name}");
    assert_eq!(
        by_name["file_id"], 1,
        "a multi-episode file re-resolves by its name"
    );

    let (status, missing) = rig
        .resolve(json!({ "release_key": key, "episode": { "season": 1, "episode": 7 } }))
        .await;
    assert!(
        !status.is_success(),
        "a pack never falls back to another episode: {missing}"
    );
    assert!(
        missing.as_str().unwrap().contains("S01E07 was not found"),
        "{missing}"
    );
    assert_eq!(
        rig.nzbs.fetches("g-pack"),
        1,
        "binge reuses the pack import"
    );
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_a_pack_resolves_each_requested_episode_with_no_search_behind_it() {
    let rig = UsenetRig::start().await;
    rig.engine.layout(
        "g-season",
        &[
            ("Engine.Season.Show.S01E04.1080p.mkv", GB),
            ("Engine.Season.Show.S01E05.1080p.mkv", GB),
            ("Engine.Season.Show.S01E06.1080p.mkv", 2 * GB),
            ("Engine.Season.Show.S01E06.1080p.en.srt", 40_000),
        ],
    );
    let nzb_ref = rig
        .list("g-season", "Engine.Season.Show.S01.1080p.WEB-DL.x264-NZB")
        .await;

    let (status, e05) = rig
        .resolve(json!({ "nzb_ref": nzb_ref, "episode": { "season": 1, "episode": 5 } }))
        .await;
    assert_eq!(status, StatusCode::OK, "{e05}");
    assert_eq!(e05["filename"], "Engine.Season.Show.S01E05.1080p.mkv");
    assert_eq!(e05["file_id"], 1);
    assert_eq!(claims(e05["stream_url"].as_str().unwrap()).output, 1);

    let key = e05["release_key"].as_str().unwrap();
    let (status, e06) = rig
        .resolve(
            json!({ "release_key": key, "episode": { "season": 1, "episode": 6 },
                         "release_title": "Engine.Season.Show.S01.1080p.WEB-DL.x264-NZB" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{e06}");
    assert_eq!(e06["filename"], "Engine.Season.Show.S01E06.1080p.mkv");
    assert_eq!(e06["file_id"], 2);

    let (status, whole) = rig.resolve(json!({ "release_key": key })).await;
    assert_eq!(status, StatusCode::OK, "{whole}");
    assert_eq!(
        whole["file_id"], 2,
        "with nothing asked for, the largest video plays"
    );
    rig.tv.server.cleanup().await;
}

/// kobbler's prewarm resolves each pickable option with `priority: warm`, built from the
/// option by `apply_option` (nzb_ref, release_key, first file, title).
fn warm_body(option: &nats_jobs::types::StreamOption) -> Value {
    json!({
        "nzb_ref": option.nzb_ref,
        "release_key": option.release_key,
        "file_id": option.files.first().map(|f| f.id),
        "release_title": option.release_title,
        "priority": "warm",
    })
}

#[tokio::test]
async fn test_warming_grabs_only_the_auto_play_pick_but_warms_imported_ones_freely() {
    let rig = UsenetRig::start().await;
    rig.tv.indexer.set_search_results(vec![
        rig.nzb("g-top", "Engine.Warm.Show.S01E05.1080p.WEB-DL.x264-NZB"),
        rig.nzb("g-low", "Engine.Warm.Show.S01E05.480p.HDTV.x264-NZB"),
    ]);
    let options = rig.options("Engine Warm Show", "5").await;
    assert_eq!(options.len(), 2, "{options:?}");
    assert!(
        options
            .iter()
            .all(|o| o.availability == Some(StreamAvailability::Likely)),
        "kobbler's pickable needs Ready or Likely: {options:?}"
    );
    let (top, low) = (&options[0], &options[1]);

    let (status, body) = rig.resolve(warm_body(low)).await;
    assert!(
        !status.is_success(),
        "warming a fallback grabbed it: {body}"
    );
    assert_eq!(rig.engine.imports_with_nzb(), 0);
    assert_eq!(rig.nzbs.fetches("g-low"), 0);

    let (status, body) = rig.resolve(warm_body(top)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (play, cache) = purposes(&body);
    assert_purpose(&play, Purpose::Play);
    assert_purpose(&cache, Purpose::Warm);
    assert_eq!(rig.nzbs.fetches("g-top"), 1);

    let (status, body) = rig.resolve(json!({ "nzb_ref": low.nzb_ref })).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a tap still imports a fallback: {body}"
    );
    assert_eq!(rig.nzbs.fetches("g-low"), 1);

    let options = rig.options("Engine Warm Show", "5").await;
    let low = options
        .iter()
        .find(|o| o.nzb_ref == Some(rig.nzb_ref("g-low")))
        .unwrap();
    assert!(low.release_key.is_some());
    let (status, body) = rig.resolve(warm_body(low)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an imported release warms freely: {body}"
    );
    assert_eq!(rig.nzbs.fetches("g-low"), 1, "and costs no grab");
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_failures_are_remembered_for_the_usenet_provider() {
    let rig = UsenetRig::start().await;
    crate::common::stream_fixtures::create_provider(&rig.tv.server.pool, "real_debrid").await;
    rig.engine.reject("g-dead");
    rig.tv.indexer.set_search_results(vec![
        rig.nzb("g-dead", "Engine.Fail.Show.S01E05.2160p.WEB-DL.x264-NZB"),
        rig.nzb("g-good", "Engine.Fail.Show.S01E05.1080p.WEB-DL.x264-NZB"),
    ]);
    let options = rig.options("Engine Fail Show", "5").await;
    assert_eq!(options.len(), 2, "{options:?}");
    let dead = options
        .iter()
        .find(|o| o.nzb_ref == Some(rig.nzb_ref("g-dead")))
        .unwrap();
    let (status, body) = rig.resolve(json!({ "nzb_ref": dead.nzb_ref })).await;
    assert!(!status.is_success(), "{body}");
    assert!(
        body.as_str().unwrap().contains("not cached")
            && body.as_str().unwrap().contains("missing_articles"),
        "{body}"
    );

    let good = options
        .iter()
        .find(|o| o.nzb_ref == Some(rig.nzb_ref("g-good")))
        .unwrap();
    let resolved = rig.resolve_option(good).await;
    let key = resolved["release_key"].as_str().unwrap();
    let resp = rig
        .tv
        .server
        .admin_client()
        .post_json(
            "/api/v1/stream/outcome",
            &json!({ "debrid_provider_id": rig.provider, "release_key": key, "ok": false,
                     "reason": "upstream_stalled" }),
        )
        .await;
    assert_eq!(resp.status, StatusCode::NO_CONTENT, "{}", resp.text());

    let options = rig.options("Engine Fail Show", "5").await;
    let by_guid = |guid: &str| {
        options
            .iter()
            .find(|o| o.nzb_ref == Some(rig.nzb_ref(guid)))
            .unwrap()
    };
    assert_eq!(
        by_guid("g-dead").availability,
        Some(StreamAvailability::Uncached),
        "{options:?}"
    );
    assert_eq!(by_guid("g-good").playback, Some(StreamPlayback::Failed));
    rig.tv.server.cleanup().await;
}
