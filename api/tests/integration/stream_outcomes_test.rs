use crate::common;

use common::TestServer;
use common::stream_fixtures::{create_provider, hash, option, unknown, with_file};
use kobload_api::hash_sources::torrentio::parse_streams;
use kobload_api::jobs::handlers::CacheCleanupHandler;
use kobload_api::jobs::worker::JobHandler;
use kobload_api::services::stream::warm_cache::UnavailableReason;
use kobload_api::services::stream::{
    KnownAvailability, RankCandidate, RankPreferences, RankProvider, outcomes, rank_options,
};
use nats_jobs::types::{StreamOption, StreamPlayback, StreamPreference};
use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

async fn ranked_titles(
    server: &TestServer,
    provider: &RankProvider,
    candidates: Vec<RankCandidate>,
    prefer: Option<&StreamPreference>,
) -> Vec<(String, Option<StreamPlayback>)> {
    rank_options(
        &server.pool,
        provider,
        candidates,
        &RankPreferences {
            prefer,
            ..Default::default()
        },
    )
    .await
    .into_iter()
    .map(|o| (o.release_title, o.playback))
    .collect()
}

fn titles(ranked: &[(String, Option<StreamPlayback>)]) -> Vec<&str> {
    ranked.iter().map(|(t, _)| t.as_str()).collect()
}

async fn report(server: &TestServer, body: serde_json::Value) {
    server
        .admin_client()
        .post_json("/api/v1/stream/outcome", &body)
        .await
        .assert_status(StatusCode::NO_CONTENT);
}

async fn row_count(pool: &sqlx::PgPool, table: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn test_outcome_endpoint_requires_auth() {
    let server = TestServer::start().await;
    server
        .anon_client()
        .post_json(
            "/api/v1/stream/outcome",
            &json!({ "info_hash": hash('a'), "ok": true }),
        )
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    server.cleanup().await;
}

#[tokio::test]
async fn test_failed_release_sinks_and_both_verdicts_are_annotated() {
    let server = TestServer::start().await;
    let provider = create_provider(&server.pool, "torbox").await;
    let options = [
        with_file(option(&provider, &hash('a'), "Broken.1080p-AAA", 9.0), 1),
        option(&provider, &hash('b'), "Untried.1080p-BBB", 8.0),
        option(&provider, &hash('c'), "Played.720p-CCC", 7.0),
        with_file(
            option(&provider, &hash('a'), "Broken.Other.File-AAA", 6.0),
            2,
        ),
    ];
    report(
        &server,
        json!({ "debrid_provider_id": provider.id, "info_hash": hash('a').to_uppercase(),
                "file_id": 1, "ok": false, "reason": "decoder_error",
                "release_title": "Broken.1080p-AAA" }),
    )
    .await;
    report(
        &server,
        json!({ "info_hash": hash('c'), "ok": true, "release_title": "Played.720p-CCC" }),
    )
    .await;

    let ranked = ranked_titles(&server, &provider, unknown(&options), None).await;
    assert_eq!(
        ranked,
        vec![
            ("Played.720p-CCC".to_string(), Some(StreamPlayback::Worked)),
            ("Untried.1080p-BBB".to_string(), None),
            ("Broken.Other.File-AAA".to_string(), None),
            ("Broken.1080p-AAA".to_string(), Some(StreamPlayback::Failed)),
        ],
        "a release that played leads; only the exact failed file sinks, not the rest of its pack"
    );
    server.cleanup().await;
}

#[tokio::test]
async fn test_release_failure_expires_and_the_option_returns_to_rank() {
    let server = TestServer::start().await;
    let provider = create_provider(&server.pool, "torbox").await;
    let options = [
        option(&provider, &hash('a'), "First.1080p-AAA", 9.0),
        option(&provider, &hash('b'), "Second.1080p-BBB", 8.0),
    ];
    report(
        &server,
        json!({ "debrid_provider_id": provider.id, "info_hash": hash('a'), "ok": false,
                "reason": "upstream_failed" }),
    )
    .await;
    assert_eq!(
        titles(&ranked_titles(&server, &provider, unknown(&options), None).await),
        vec!["Second.1080p-BBB", "First.1080p-AAA"]
    );

    sqlx::query("UPDATE stream_release_outcomes SET last_failure_at = NOW() - INTERVAL '4 days'")
        .execute(&server.pool)
        .await
        .unwrap();
    assert_eq!(
        ranked_titles(&server, &provider, unknown(&options), None).await,
        vec![
            ("First.1080p-AAA".to_string(), None),
            ("Second.1080p-BBB".to_string(), None)
        ],
        "a failure older than its memory must be retried"
    );
    server.cleanup().await;
}

#[tokio::test]
async fn test_transient_failures_are_never_recorded() {
    let server = TestServer::start().await;
    let provider = create_provider(&server.pool, "real_debrid").await;
    for reason in [
        "rate_limited",
        "auth_failed",
        "timeout",
        "unavailable",
        "fair_usage",
        "too_many_active",
        "Rate limited (retry after 5s)",
    ] {
        report(
            &server,
            json!({ "debrid_provider_id": provider.id, "info_hash": hash('a'), "ok": false,
                    "reason": reason, "release_title": "Show.S01E01.WEB-DL-GRP" }),
        )
        .await;
    }
    assert_eq!(row_count(&server.pool, "stream_release_outcomes").await, 0);
    assert_eq!(row_count(&server.pool, "stream_trait_outcomes").await, 0);
    server.cleanup().await;
}

#[tokio::test]
async fn test_trait_learning_shifts_rank_softly_and_decays() {
    let server = TestServer::start().await;
    let provider = create_provider(&server.pool, "real_debrid").await;
    let options = [
        option(
            &provider,
            &hash('a'),
            "Show.S01E05.1080p.BluRay.x264-FLAKY",
            9.0,
        ),
        option(
            &provider,
            &hash('b'),
            "Show.S01E05.1080p.BluRay.x264-STEADY",
            8.0,
        ),
    ];
    let cold = ranked_titles(&server, &provider, unknown(&options), None).await;
    assert_eq!(titles(&cold)[0], "Show.S01E05.1080p.BluRay.x264-FLAKY");

    for i in 0..3 {
        outcomes::record_probe(
            &server.pool,
            provider.id,
            &format!("{:040x}", 50 + i),
            Some(&format!("Show.S01E0{i}.1080p.BluRay.x264-OK{i}")),
            None,
        )
        .await;
    }
    for (i, c) in ['1', '2', '3'].into_iter().enumerate() {
        outcomes::record_probe(
            &server.pool,
            provider.id,
            &hash(c),
            Some(&format!("Show.S01E0{i}.1080p.BluRay.x264-FLAKY")),
            Some(UnavailableReason::Blocked),
        )
        .await;
    }
    assert_eq!(
        titles(&ranked_titles(&server, &provider, unknown(&options), None).await),
        vec![
            "Show.S01E05.1080p.BluRay.x264-STEADY",
            "Show.S01E05.1080p.BluRay.x264-FLAKY"
        ],
        "a group the provider keeps refusing drops below its peers"
    );

    let mut tiered = unknown(&options);
    tiered[0].availability = KnownAvailability::Available;
    assert_eq!(
        titles(&ranked_titles(&server, &provider, tiered, None).await),
        vec![
            "Show.S01E05.1080p.BluRay.x264-FLAKY",
            "Show.S01E05.1080p.BluRay.x264-STEADY"
        ],
        "learned risk never outranks first-hand availability, and never excludes"
    );

    sqlx::query("UPDATE stream_trait_outcomes SET updated_at = NOW() - INTERVAL '30 days'")
        .execute(&server.pool)
        .await
        .unwrap();
    assert_eq!(
        titles(&ranked_titles(&server, &provider, unknown(&options), None).await),
        titles(&cold),
        "evidence decays back to the prior"
    );
    server.cleanup().await;
}

#[tokio::test]
async fn test_repeated_probe_verdicts_count_once() {
    let server = TestServer::start().await;
    let provider = create_provider(&server.pool, "real_debrid").await;
    for _ in 0..3 {
        outcomes::record_probe(
            &server.pool,
            provider.id,
            &hash('a'),
            Some("Movie.2024.1080p.WEB-DL-GRP"),
            Some(UnavailableReason::Blocked),
        )
        .await;
    }
    let weight: f64 = sqlx::query_scalar(
        "SELECT failure_weight FROM stream_trait_outcomes WHERE trait = 'group:grp'",
    )
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert!(
        (weight - 1.0).abs() < 0.01,
        "re-probing a known verdict is not new evidence, got {weight}"
    );
    server.cleanup().await;
}

#[test]
fn test_binge_group_parsed_from_torrentio_fixture() {
    let body = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/torrentio_series.json"
    ))
    .unwrap();
    let parsed = parse_streams(&body, "tt0000001", "tv", Some(1), Some(2)).unwrap();
    let got: Vec<(String, Option<String>)> = parsed
        .into_iter()
        .map(|r| (r.entry.info_hash, r.binge_group))
        .collect();
    assert_eq!(
        got,
        vec![
            (hash('a'), Some(format!("torrentio|{}", hash('a')))),
            (hash('b'), Some("torrentio|720p|HDTV".to_string())),
            (hash('c'), None),
        ]
    );
}

#[tokio::test]
async fn test_continuity_prefers_same_pack_then_binge_group_then_release_group() {
    let server = TestServer::start().await;
    let provider = create_provider(&server.pool, "torbox").await;
    // Ranking remembers binge groups process-wide by hash; shared fixture hashes would leak
    // these groups into other tests in the integration binary.
    let salt = Uuid::new_v4().simple().to_string();
    let hash = |c: char| format!("{}{salt}", c.to_string().repeat(8));
    let binge = |mut o: StreamOption, group: &str| {
        o.binge_group = Some(group.to_string());
        o
    };
    let episode_one = [
        binge(
            with_file(
                option(&provider, &hash('p'), "Show.S01.1080p.WEB-PACK", 5.0),
                1,
            ),
            "torrentio|pack",
        ),
        binge(
            option(&provider, &hash('e'), "Show.S01E01.720p.HDTV-LOL", 5.0),
            "torrentio|720p|HDTV",
        ),
    ];
    rank_options(
        &server.pool,
        &provider,
        unknown(&episode_one),
        &RankPreferences::default(),
    )
    .await;

    let episode_two = [
        option(&provider, &hash('x'), "Show.S01E02.2160p.WEB-TOP", 9.0),
        binge(
            option(&provider, &hash('y'), "Show.S01E02.720p.HDTV-OTHER", 3.0),
            "torrentio|720p|HDTV",
        ),
        option(&provider, &hash('z'), "Show.S01E02.480p.HDTV-LOL", 2.0),
        binge(
            with_file(
                option(&provider, &hash('p'), "Show.S01.1080p.WEB-PACK", 1.0),
                2,
            ),
            "torrentio|pack",
        ),
    ];
    let cases = [
        (
            StreamPreference {
                info_hash: hash('p'),
                file_id: Some(1),
                release_title: Some("Show.S01.1080p.WEB-PACK".to_string()),
            },
            "Show.S01.1080p.WEB-PACK",
        ),
        (
            StreamPreference {
                info_hash: hash('e'),
                file_id: None,
                release_title: Some("Show.S01E01.720p.HDTV-UNRELATED".to_string()),
            },
            "Show.S01E02.720p.HDTV-OTHER",
        ),
        (
            StreamPreference {
                info_hash: hash('q'),
                file_id: None,
                release_title: Some("Show.S01E01.480p.HDTV-LOL".to_string()),
            },
            "Show.S01E02.480p.HDTV-LOL",
        ),
        (
            StreamPreference {
                info_hash: hash('q'),
                file_id: None,
                release_title: None,
            },
            "Show.S01E02.2160p.WEB-TOP",
        ),
    ];
    for (prefer, want) in cases {
        let ranked = ranked_titles(&server, &provider, unknown(&episode_two), Some(&prefer)).await;
        assert_eq!(titles(&ranked)[0], want, "prefer {prefer:?}");
    }

    let mut tiered = unknown(&episode_two);
    tiered[0].availability = KnownAvailability::Available;
    let prefer = StreamPreference {
        info_hash: hash('e'),
        file_id: None,
        release_title: None,
    };
    assert_eq!(
        titles(&ranked_titles(&server, &provider, tiered, Some(&prefer)).await)[0],
        "Show.S01E02.2160p.WEB-TOP",
        "binge-group continuity is soft: a verified-available release still leads"
    );
    server.cleanup().await;
}

#[tokio::test]
async fn test_cleanup_job_prunes_expired_outcomes() {
    let server = TestServer::start().await;
    let provider = create_provider(&server.pool, "real_debrid").await;
    for (c, ok) in [('a', false), ('b', true), ('c', false), ('d', true)] {
        report(
            &server,
            json!({ "debrid_provider_id": provider.id, "info_hash": hash(c), "ok": ok,
                    "reason": "decoder_error", "release_title": format!("Movie.{c}.BluRay-G{c}") }),
        )
        .await;
    }
    sqlx::query(
        "UPDATE stream_release_outcomes SET
            last_failure_at = last_failure_at - INTERVAL '4 days',
            last_success_at = last_success_at - INTERVAL '31 days'
         WHERE info_hash = ANY($1)",
    )
    .bind(vec![hash('a'), hash('b')])
    .execute(&server.pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE stream_trait_outcomes SET updated_at = NOW() - INTERVAL '90 days'
         WHERE trait IN ('group:ga', 'group:gb')",
    )
    .execute(&server.pool)
    .await
    .unwrap();

    CacheCleanupHandler::new(server.pool.clone())
        .execute(
            Uuid::new_v4(),
            json!({}),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();

    let hashes: Vec<String> =
        sqlx::query_scalar("SELECT info_hash FROM stream_release_outcomes ORDER BY info_hash")
            .fetch_all(&server.pool)
            .await
            .unwrap();
    assert_eq!(hashes, vec![hash('c'), hash('d')]);
    let traits: Vec<String> = sqlx::query_scalar(
        "SELECT trait FROM stream_trait_outcomes WHERE trait LIKE 'group:%' ORDER BY trait",
    )
    .fetch_all(&server.pool)
    .await
    .unwrap();
    assert_eq!(
        traits,
        vec!["group:gd"],
        "playback failures teach nothing about refusals"
    );
    server.cleanup().await;
}
