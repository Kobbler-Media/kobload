//! Config repository: Application settings.

use super::Result;
use sqlx::{Executor, Postgres};

/// Prowlarr configuration extracted from an indexer_source.
#[derive(Debug, Clone)]
pub struct ProwlarrConfigRow {
    pub source_id: uuid::Uuid,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
}

/// Get the first enabled Prowlarr indexer source's config.
///
/// Returns `None` if no `handler_type = 'prowlarr'` source exists.
pub async fn get_prowlarr_config<'e, E>(exec: E) -> Result<Option<ProwlarrConfigRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query!(
        "SELECT id, handler_config FROM indexer_sources WHERE handler_type = 'prowlarr' AND enabled = TRUE LIMIT 1",
    )
    .fetch_optional(exec)
    .await?;

    Ok(row.map(|r| ProwlarrConfigRow {
        source_id: r.id,
        base_url: r
            .handler_config
            .get("base_url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        api_key: r
            .handler_config
            .get("api_key")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    }))
}
