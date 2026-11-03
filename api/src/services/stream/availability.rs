use std::collections::HashMap;

use sqlx::PgPool;
use uuid::Uuid;

use super::ranking::KnownAvailability;

/// How long a successful probe stays trusted. RD evicts slowly (Torrentio trusts its
/// equivalent for 14 days), a stale verdict only costs one failed resolve in the walk,
/// and every re-probe spends one of RD's scarce adds.
const AVAILABLE_TTL: &str = "7 days";
/// Dead torrents can pick up seeders again, so re-probe them daily.
const DEAD_TTL: &str = "24 hours";
/// The RD filter-gate verdict only demotes, and a copy on the account overrides it.
const BLOCKED_TTL: &str = "24 hours";

/// Look up what we already know about these hashes on the given provider.
///
/// Applies a different freshness window per outcome.
pub async fn known_availability(
    pool: &PgPool,
    provider_id: Uuid,
    hashes: &[String],
) -> HashMap<String, KnownAvailability> {
    if hashes.is_empty() {
        return HashMap::new();
    }
    // TTLs are interpolated from the consts above — they are compile-time
    // literals, never user input, so this cannot carry untrusted SQL.
    let rows = sqlx::query_as::<_, (String, bool, Option<String>)>(sqlx::AssertSqlSafe(format!(
        r#"SELECT info_hash, available, reason FROM debrid_availability_cache
           WHERE provider_id = $1
             AND info_hash = ANY($2)
             AND checked_at > NOW() - CASE
                 WHEN available THEN INTERVAL '{AVAILABLE_TTL}'
                 WHEN reason = 'blocked' THEN INTERVAL '{BLOCKED_TTL}'
                 ELSE INTERVAL '{DEAD_TTL}'
             END"#
    )))
    .bind(provider_id)
    .bind(hashes)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .map(|(hash, available, reason)| {
            let state = if available {
                KnownAvailability::Available
            } else if reason.as_deref() == Some("blocked") {
                KnownAvailability::Blocked
            } else {
                KnownAvailability::Dead
            };
            (hash, state)
        })
        .collect()
}
