use crate::common;

use common::TestServer;
use common::stream_fixtures::{create_provider, hash, option};
use kobload_api::jobs::handlers::CacheCleanupHandler;
use kobload_api::jobs::worker::JobHandler;
use kobload_api::services::stream::warm_cache::UnavailableReason;
use kobload_api::services::stream::{
    KnownAvailability, RankCandidate, RankPreferences, RankProvider, outcomes, rank_options,
};
use uuid::Uuid;

const TAGGED: &str = "Movie.2024.1080p.ZQX.x264-AAA";
const PLAIN: &str = "Movie.2024.1080p.BluRay.x264-BBB";

/// A tagged release the provider is said to hold, against a plain one nobody has checked.
/// Without learned risk the tagged one leads on tier; once refusals of its tag are
/// expected it drops below the unchecked one.
async fn leader(server: &TestServer, provider: &RankProvider) -> String {
    let candidates = vec![
        RankCandidate {
            option: option(provider, &hash('a'), TAGGED, 9.0),
            availability: KnownAvailability::LikelyAvailable,
        },
        RankCandidate {
            option: option(provider, &hash('b'), PLAIN, 8.0),
            availability: KnownAvailability::Unknown,
        },
    ];
    rank_options(
        &server.pool,
        provider,
        candidates,
        &RankPreferences::default(),
    )
    .await
    .remove(0)
    .release_title
}

async fn refuse(server: &TestServer, provider: Uuid, title: &str, n: usize) {
    for i in 0..n {
        outcomes::record_probe(
            &server.pool,
            provider,
            &format!("{:040x}", i + 1000 * title.len()),
            Some(&format!("Other.{i}.2024.{title}-G{i}")),
            Some(UnavailableReason::Blocked),
        )
        .await;
    }
}

/// Acceptances of unrelated releases, so the provider has a baseline to stand a tag against:
/// a provider that refuses everything has no tag worth avoiding.
async fn accept_others(server: &TestServer, provider: Uuid, n: usize) {
    for i in 0..n {
        outcomes::record_probe(
            &server.pool,
            provider,
            &format!("{:040x}", 70_000 + i),
            Some(&format!("Fine.{i}.2024.HDR-H{i}")),
            None,
        )
        .await;
    }
}

async fn age_traits(server: &TestServer, provider: Uuid, days: i32) {
    sqlx::query(
        "UPDATE stream_trait_outcomes SET updated_at = NOW() - make_interval(days => $2)
         WHERE provider_id = $1",
    )
    .bind(provider)
    .bind(days)
    .execute(&server.pool)
    .await
    .unwrap();
}

async fn cleanup_job(server: &TestServer) {
    CacheCleanupHandler::new(server.pool.clone())
        .execute(
            Uuid::new_v4(),
            serde_json::json!({}),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
}

async fn trait_rows(server: &TestServer, provider: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM stream_trait_outcomes WHERE provider_id = $1")
        .bind(provider)
        .fetch_one(&server.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn test_a_new_provider_ranks_neutrally() {
    let server = TestServer::start().await;
    for provider_type in ["real_debrid", "torbox"] {
        let p = create_provider(&server.pool, provider_type).await;
        let candidates = ["Movie.2024.1080p.AMZN.WEB-DL.DDP5.1-AAA", PLAIN]
            .iter()
            .enumerate()
            .map(|(i, t)| RankCandidate {
                option: option(&p, &hash(['a', 'b'][i]), t, 9.0 - i as f64),
                availability: KnownAvailability::Unknown,
            })
            .collect();
        let ranked = rank_options(&server.pool, &p, candidates, &RankPreferences::default()).await;
        assert_eq!(
            ranked[0].release_title, "Movie.2024.1080p.AMZN.WEB-DL.DDP5.1-AAA",
            "{provider_type}: no evidence, no penalty"
        );
    }
    server.cleanup().await;
}

#[tokio::test]
async fn test_refusals_of_a_made_up_tag_teach_the_ranking() {
    let server = TestServer::start().await;
    let p = create_provider(&server.pool, "real_debrid").await;
    accept_others(&server, p.id, 6).await;
    refuse(&server, p.id, "ZQX", 2).await;
    assert_eq!(
        leader(&server, &p).await,
        TAGGED,
        "below the evidence floor"
    );
    refuse(&server, p.id, "ZQX", 6).await;
    assert_eq!(leader(&server, &p).await, PLAIN);

    let other = create_provider(&server.pool, "real_debrid").await;
    assert_eq!(
        leader(&server, &other).await,
        TAGGED,
        "evidence belongs to the provider that refused"
    );
    server.cleanup().await;
}

#[tokio::test]
async fn test_spelling_variants_are_learned_apart() {
    let server = TestServer::start().await;
    let p = create_provider(&server.pool, "real_debrid").await;
    refuse(&server, p.id, "QQ-ZZ", 6).await;
    for i in 0..6 {
        outcomes::record_probe(
            &server.pool,
            p.id,
            &format!("{:040x}", 9000 + i),
            Some(&format!("Other.{i}.2024.QQZZ-H{i}")),
            None,
        )
        .await;
    }
    let rank = |title: &'static str| {
        let p = &p;
        let server = &server;
        async move {
            let candidates = vec![
                RankCandidate {
                    option: option(p, &hash('a'), title, 9.0),
                    availability: KnownAvailability::LikelyAvailable,
                },
                RankCandidate {
                    option: option(p, &hash('b'), PLAIN, 8.0),
                    availability: KnownAvailability::Unknown,
                },
            ];
            rank_options(&server.pool, p, candidates, &RankPreferences::default())
                .await
                .remove(0)
                .release_title
        }
    };
    assert_eq!(
        rank("Movie.2024.1080p.QQZZ-AAA").await,
        "Movie.2024.1080p.QQZZ-AAA"
    );
    assert_eq!(rank("Movie.2024.1080p.QQ-ZZ-AAA").await, PLAIN);
    server.cleanup().await;
}

#[tokio::test]
async fn test_decay_fades_influence() {
    let server = TestServer::start().await;
    let p = create_provider(&server.pool, "real_debrid").await;
    accept_others(&server, p.id, 6).await;
    refuse(&server, p.id, "ZQX", 6).await;
    assert_eq!(leader(&server, &p).await, PLAIN);
    age_traits(&server, p.id, 15).await;
    assert_eq!(
        leader(&server, &p).await,
        TAGGED,
        "past a half-life too little evidence is left to judge the tag"
    );
    server.cleanup().await;
}

#[tokio::test]
async fn test_stale_evidence_is_evicted() {
    let server = TestServer::start().await;
    let p = create_provider(&server.pool, "real_debrid").await;
    accept_others(&server, p.id, 6).await;
    refuse(&server, p.id, "ZQX", 20).await;
    assert_eq!(leader(&server, &p).await, PLAIN);
    age_traits(&server, p.id, outcomes::TRAIT_TTL_DAYS + 1).await;
    assert_eq!(
        leader(&server, &p).await,
        TAGGED,
        "expired rows are not read"
    );
    cleanup_job(&server).await;
    assert_eq!(trait_rows(&server, p.id).await, 0);
    server.cleanup().await;
}

#[tokio::test]
async fn test_past_probes_are_ingested_once() {
    let server = TestServer::start().await;
    let p = create_provider(&server.pool, "real_debrid").await;
    for i in 0..6 {
        let h = format!("{:040x}", 500 + i);
        sqlx::query("INSERT INTO torrent_hash_index (info_hash, original_title) VALUES ($1, $2)")
            .bind(&h)
            .bind(format!("Old.{i}.2024.ZQX-G{i}"))
            .execute(&server.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO debrid_availability_cache (info_hash, provider_id, available, reason, checked_at)
             VALUES ($1, $2, false, 'blocked', NOW() - INTERVAL '1 day')",
        )
        .bind(&h)
        .bind(p.id)
        .execute(&server.pool)
        .await
        .unwrap();
    }
    for i in 0..6 {
        let h = format!("{:040x}", 600 + i);
        sqlx::query("INSERT INTO torrent_hash_index (info_hash, original_title) VALUES ($1, $2)")
            .bind(&h)
            .bind(format!("Fine.{i}.2024.HDR-H{i}"))
            .execute(&server.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO debrid_availability_cache (info_hash, provider_id, available, checked_at)
             VALUES ($1, $2, true, NOW() - INTERVAL '1 day')",
        )
        .bind(&h)
        .bind(p.id)
        .execute(&server.pool)
        .await
        .unwrap();
    }
    assert_eq!(leader(&server, &p).await, TAGGED);
    cleanup_job(&server).await;
    assert_eq!(leader(&server, &p).await, PLAIN);

    let weight = || async {
        sqlx::query_scalar::<_, f64>(
            "SELECT failure_weight FROM stream_trait_outcomes WHERE provider_id = $1 AND trait = 'tok:zqx'",
        )
        .bind(p.id)
        .fetch_one(&server.pool)
        .await
        .unwrap()
    };
    let once = weight().await;
    cleanup_job(&server).await;
    assert!(
        (weight().await - once).abs() < 0.01,
        "a second run adds nothing"
    );
    server.cleanup().await;
}
