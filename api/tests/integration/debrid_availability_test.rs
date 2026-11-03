//! Availability-cache recording is keyed by info_hash, and every reader
//! lowercases before it looks up. These cover the write side agreeing.

use crate::common;

use common::TestServer;
use kobload_api::services::stream::warm_cache::{UnavailableReason, record_availability};

const MIXED: &str = "AABBCCDDEEFF00112233445566778899AABBCCDD";
const LOWER: &str = "aabbccddeeff00112233445566778899aabbccdd";

async fn create_provider(pool: &sqlx::PgPool) -> uuid::Uuid {
    sqlx::query_scalar!(
        r#"INSERT INTO debrid_providers (name, provider_type, api_key)
           VALUES ('test', 'real_debrid', 'k') RETURNING id"#
    )
    .fetch_one(pool)
    .await
    .expect("create provider")
}

async fn stored_hashes(pool: &sqlx::PgPool, provider_id: uuid::Uuid) -> Vec<String> {
    sqlx::query_scalar!(
        "SELECT info_hash FROM debrid_availability_cache WHERE provider_id = $1 ORDER BY info_hash",
        provider_id
    )
    .fetch_all(pool)
    .await
    .expect("read cache")
}

#[tokio::test]
async fn test_record_availability_normalises_hash_case() {
    let server = TestServer::start().await;
    let provider_id = create_provider(&server.pool).await;

    record_availability(
        &server.pool,
        MIXED,
        provider_id,
        Some(UnavailableReason::Blocked),
    )
    .await;

    assert_eq!(
        stored_hashes(&server.pool, provider_id).await,
        vec![LOWER.to_string()],
        "a mixed-case hash must be stored lowercase, or no reader will ever match it"
    );

    server.cleanup().await;
}

#[tokio::test]
async fn test_record_availability_case_variants_share_a_row() {
    let server = TestServer::start().await;
    let provider_id = create_provider(&server.pool).await;

    record_availability(
        &server.pool,
        MIXED,
        provider_id,
        Some(UnavailableReason::Blocked),
    )
    .await;
    record_availability(&server.pool, LOWER, provider_id, None).await;

    assert_eq!(
        stored_hashes(&server.pool, provider_id).await,
        vec![LOWER.to_string()],
        "case variants of one hash must not become two rows"
    );

    let available = sqlx::query_scalar!(
        "SELECT available FROM debrid_availability_cache WHERE provider_id = $1 AND info_hash = $2",
        provider_id,
        LOWER
    )
    .fetch_one(&server.pool)
    .await
    .expect("read row");
    assert!(available, "the later probe must win the shared row");

    server.cleanup().await;
}

/// RD evicts cached torrents slowly and a stale verdict is recovered by the walk, while
/// every re-probe spends one of RD's scarce adds; a refusal (usually RD pausing the
/// account's adds) or a dead swarm ages faster.
#[tokio::test]
async fn test_verdicts_stay_trusted_for_their_window() {
    use kobload_api::services::stream::{KnownAvailability, known_availability};
    let server = TestServer::start().await;
    let provider_id = create_provider(&server.pool).await;
    let cases = [
        ('a', None, 2, Some(KnownAvailability::Available)),
        ('b', None, 8, None),
        ('c', Some(UnavailableReason::Dead), 2, None),
        ('d', Some(UnavailableReason::Blocked), 2, None),
    ];
    for (c, reason, days, _) in &cases {
        let hash = c.to_string().repeat(40);
        record_availability(&server.pool, &hash, provider_id, *reason).await;
        sqlx::query("UPDATE debrid_availability_cache SET checked_at = NOW() - make_interval(days => $1) WHERE info_hash = $2")
            .bind(*days)
            .bind(&hash)
            .execute(&server.pool)
            .await
            .unwrap();
    }
    let hashes: Vec<String> = cases
        .iter()
        .map(|(c, ..)| c.to_string().repeat(40))
        .collect();
    let known = known_availability(&server.pool, provider_id, &hashes).await;
    for ((c, _, days, expected), hash) in cases.iter().zip(&hashes) {
        assert_eq!(known.get(hash), expected.as_ref(), "{c} after {days} days");
    }
    server.cleanup().await;
}
