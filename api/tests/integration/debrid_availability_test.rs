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
