use std::collections::HashMap;
use std::time::Duration;

use sqlx::PgPool;
use uuid::Uuid;

use super::ranking::KnownAvailability;
use super::warm_cache::VERDICT_FRESHNESS;

/// How long a successful probe stays trusted. RD evicts slowly (Torrentio trusts its
/// equivalent for 14 days), a stale verdict only costs one failed resolve in the walk,
/// and every re-probe spends one of RD's scarce adds.
const AVAILABLE_TTL: &str = "7 days";
/// Dead torrents can pick up seeders again, so re-probe them daily.
const DEAD_TTL: Duration = Duration::from_secs(24 * 3600);
/// The RD filter-gate verdict only demotes, and a copy on the account overrides it.
const BLOCKED_TTL: &str = "24 hours";

/// Look up what we already know about these releases on the given provider.
///
/// Applies a different freshness window per outcome.
pub async fn known_availability(
    pool: &PgPool,
    provider_id: Uuid,
    release_keys: &[String],
) -> HashMap<String, KnownAvailability> {
    if release_keys.is_empty() {
        return HashMap::new();
    }
    let dead_ttl_secs = DEAD_TTL.as_secs();
    // TTLs are interpolated from the consts above — they are compile-time
    // literals, never user input, so this cannot carry untrusted SQL.
    let rows = sqlx::query_as::<_, (String, bool, Option<String>)>(sqlx::AssertSqlSafe(format!(
        r#"SELECT release_key, available, reason FROM debrid_availability_cache
           WHERE provider_id = $1
             AND release_key = ANY($2)
             AND checked_at > NOW() - CASE
                 WHEN available THEN INTERVAL '{AVAILABLE_TTL}'
                 WHEN reason = 'blocked' THEN INTERVAL '{BLOCKED_TTL}'
                 ELSE INTERVAL '{dead_ttl_secs} seconds'
             END"#
    )))
    .bind(provider_id)
    .bind(release_keys)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .map(|(key, available, reason)| {
            let state = if available {
                KnownAvailability::Available
            } else if reason.as_deref() == Some("blocked") {
                KnownAvailability::Blocked
            } else {
                KnownAvailability::Dead
            };
            (key, state)
        })
        .collect()
}

/// An import proves the articles it read exist, not that the rest of the release does, so
/// it never revives a release found dead since. Returns whether the verdict is news.
pub async fn record_available_unless_dead(
    pool: &PgPool,
    release_key: &str,
    provider_id: Uuid,
) -> bool {
    let release_key = release_key.to_lowercase();
    let row = sqlx::query_scalar!(
        r#"INSERT INTO debrid_availability_cache AS c (release_key, provider_id, available, reason, checked_at)
           VALUES ($1, $2, true, NULL, NOW())
           ON CONFLICT (release_key, provider_id) DO UPDATE
           SET available = true, reason = NULL, checked_at = NOW()
           WHERE c.available OR c.reason IS DISTINCT FROM 'dead'
              OR c.checked_at < NOW() - make_interval(secs => $3)
           RETURNING (old.checked_at IS NULL
                      OR NOT old.available
                      OR old.checked_at < NOW() - make_interval(secs => $4)) AS "fresh!""#,
        release_key,
        provider_id,
        DEAD_TTL.as_secs_f64(),
        VERDICT_FRESHNESS.as_secs_f64(),
    )
    .fetch_optional(pool)
    .await;
    match row {
        Ok(fresh) => fresh.unwrap_or(false),
        Err(e) => {
            tracing::warn!(error = %e, "Failed to record usenet import availability");
            false
        }
    }
}
