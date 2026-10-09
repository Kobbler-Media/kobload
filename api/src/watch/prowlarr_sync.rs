//! Prowlarr indexer sync: imports indexer sources from Prowlarr.

use std::collections::HashMap;

use sqlx::PgPool;
use tracing::info;
use uuid::Uuid;

use crate::prowlarr::ProwlarrClient;
use crate::repositories::watch as watch_repo;

/// Result of syncing Prowlarr indexers
#[derive(Debug)]
pub struct SyncProwlarrResult {
    pub created: i32,
    pub updated: i32,
    pub disabled: i32,
    pub indexers: Vec<String>,
}

/// Sync indexer sources from a Prowlarr instance.
///
/// Reads connection details from the given indexer_source's handler_config.
/// Creates/updates `prowlarr_rss` entries for each enabled Prowlarr indexer.
pub async fn sync_prowlarr_indexers(
    pool: &PgPool,
    http_client: &reqwest::Client,
    prowlarr_source_id: Uuid,
) -> Result<SyncProwlarrResult, String> {
    let source = watch_repo::get_indexer_source_by_id(pool, prowlarr_source_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("Prowlarr indexer source not found")?;

    let base_url = source
        .handler_config
        .get("base_url")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("Prowlarr base URL not configured")?
        .to_string();
    let api_key = source
        .handler_config
        .get("api_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("Prowlarr API key not configured")?
        .to_string();

    let client =
        ProwlarrClient::new_with_client(base_url.clone(), api_key.clone(), http_client.clone());
    let indexers = client
        .list_indexers()
        .await
        .map_err(|e| format!("Failed to fetch Prowlarr indexers: {}", e))?;

    info!(
        indexer_count = indexers.len(),
        "Fetched indexers from Prowlarr for sync"
    );

    // Find existing prowlarr_rss sources that belong to this Prowlarr instance
    // (matching base_url in handler_config)
    let existing = watch_repo::list_indexer_sources_by_type(pool, "prowlarr_rss")
        .await
        .map_err(|e| e.to_string())?;

    let mut existing_by_indexer_id: HashMap<i32, watch_repo::IndexerSourceRow> = HashMap::new();
    for source in existing {
        let source_base_url = source
            .handler_config
            .get("base_url")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if source_base_url != base_url {
            continue; // Belongs to a different Prowlarr instance
        }
        if let Some(indexer_id) = source
            .handler_config
            .get("indexer_id")
            .or_else(|| source.handler_config.get("prowlarr_indexer_id"))
            .and_then(|v| v.as_i64())
        {
            existing_by_indexer_id.insert(indexer_id as i32, source);
        }
    }

    let mut created = 0;
    let mut updated = 0;
    let mut disabled = 0;
    let mut indexer_names = Vec::new();

    for indexer in &indexers {
        if !indexer.enable {
            continue;
        }
        if !indexer.supports_rss {
            info!(
                indexer_id = indexer.id,
                indexer_name = %indexer.name,
                "Skipping indexer without RSS support"
            );
            continue;
        }

        let category_ids: Vec<i32> = indexer.categories.iter().map(|cat| cat.id).collect();

        let handler_config = serde_json::json!({
            "base_url": base_url,
            "api_key": api_key,
            "indexer_id": indexer.id,
            "poll_interval_minutes": 15,
        });

        indexer_names.push(indexer.name.clone());

        let category_ids_opt = if category_ids.is_empty() {
            None
        } else {
            Some(category_ids)
        };

        if let Some(existing_source) = existing_by_indexer_id.remove(&indexer.id) {
            watch_repo::update_indexer_source_from_sync(
                pool,
                existing_source.id,
                &indexer.name,
                &handler_config,
                category_ids_opt.as_ref(),
            )
            .await
            .map_err(|e| e.to_string())?;

            updated += 1;
        } else {
            watch_repo::create_indexer_source_from_sync(
                pool,
                &indexer.name,
                &handler_config,
                category_ids_opt.as_ref(),
            )
            .await
            .map_err(|e| e.to_string())?;

            created += 1;
        }
    }

    for (_, source) in existing_by_indexer_id {
        watch_repo::disable_indexer_source(pool, source.id)
            .await
            .map_err(|e| e.to_string())?;

        disabled += 1;
    }

    info!(
        created = created,
        updated = updated,
        disabled = disabled,
        "Synced Prowlarr indexers"
    );

    Ok(SyncProwlarrResult {
        created,
        updated,
        disabled,
        indexers: indexer_names,
    })
}
