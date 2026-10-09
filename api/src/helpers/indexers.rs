use sqlx::PgPool;
use uuid::Uuid;

use crate::repositories::{
    handler_type_defaults, indexer_category_mappings as category_repo, watch as watch_repo,
};
use crate::services::downloads::DownloadUrlContext;

pub fn extract_indexer_source_id(release: &serde_json::Value) -> Option<Uuid> {
    release
        .get("indexer_source_id")
        .and_then(|value| value.as_str())
        .and_then(|value| Uuid::parse_str(value).ok())
}

pub async fn download_url_context_for_indexer_source(
    pool: &PgPool,
    indexer_source_id: Option<Uuid>,
) -> Result<DownloadUrlContext, String> {
    if let Some(source_id) = indexer_source_id {
        let source = watch_repo::get_indexer_source_by_id(pool, source_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "indexer source not found".to_string())?;

        match source.handler_type.as_str() {
            "newznab" => {
                let base_url = source
                    .handler_config
                    .get("base_url")
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string());
                return Ok(DownloadUrlContext {
                    base_url,
                    auth_header: None,
                    cookie: None,
                    seed_ratio: source.seed_ratio,
                    seed_time_minutes: source.seed_time_minutes,
                });
            }
            "prowlarr" | "prowlarr_rss" => {
                let base_url = source
                    .handler_config
                    .get("base_url")
                    .or_else(|| source.handler_config.get("prowlarr_base_url"))
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string());
                let api_key = source
                    .handler_config
                    .get("api_key")
                    .or_else(|| source.handler_config.get("prowlarr_api_key"))
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string());

                let base_url = base_url.filter(|value| !value.trim().is_empty());
                let api_key = api_key.filter(|value| !value.trim().is_empty());

                if base_url.is_none() || api_key.is_none() {
                    return Err("prowlarr config missing for indexer source".to_string());
                }

                return Ok(DownloadUrlContext {
                    base_url,
                    auth_header: api_key.map(|key| ("X-Api-Key".to_string(), key)),
                    cookie: None,
                    seed_ratio: source.seed_ratio,
                    seed_time_minutes: source.seed_time_minutes,
                });
            }
            "mam" => {
                let mam_id = source
                    .handler_config
                    .get("mam_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                return Ok(DownloadUrlContext {
                    base_url: None,
                    auth_header: None,
                    cookie: if mam_id.is_empty() {
                        None
                    } else {
                        Some(format!("mam_id={mam_id}"))
                    },
                    seed_ratio: source.seed_ratio,
                    seed_time_minutes: source.seed_time_minutes,
                });
            }
            "torrentday" => {
                let cookie = source
                    .handler_config
                    .get("cookie")
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_string());
                let base_url = source
                    .handler_config
                    .get("base_url")
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_string());
                return Ok(DownloadUrlContext {
                    base_url,
                    auth_header: None,
                    cookie,
                    seed_ratio: source.seed_ratio,
                    seed_time_minutes: source.seed_time_minutes,
                });
            }
            _ => {
                return Ok(DownloadUrlContext {
                    seed_ratio: source.seed_ratio,
                    seed_time_minutes: source.seed_time_minutes,
                    ..Default::default()
                });
            }
        }
    }

    // No indexer_source_id — return default (no auth, no base URL)
    Ok(DownloadUrlContext::default())
}

/// Resolve default category IDs for a media type without a specific source.
///
/// Falls back to "newznab" handler_type_defaults since most indexers use
/// the standard Newznab category ID scheme.
pub async fn resolve_categories_for_media_type(
    pool: &PgPool,
    media_type_id: Option<Uuid>,
) -> Result<Vec<i32>, String> {
    let Some(mt_id) = media_type_id else {
        return Ok(Vec::new());
    };

    // Use newznab defaults — Prowlarr speaks the same category ID scheme
    let defaults = handler_type_defaults::get_categories(pool, "newznab", mt_id)
        .await
        .map_err(|e| e.to_string())?;

    if let Some(cat_strings) = defaults {
        let ids: Vec<i32> = cat_strings
            .iter()
            .filter_map(|s| s.parse::<i32>().ok())
            .collect();
        if !ids.is_empty() {
            return Ok(ids);
        }
    }

    Ok(Vec::new())
}

/// Check if a release's categories match the allowed categories for a media type.
/// Returns true if categories match OR if no filtering is configured (empty allowed list).
///
/// Newznab categories are hierarchical: parent categories are multiples of 1000
/// (e.g. 2000=Movies, 5000=TV) and subcategories fall within that range
/// (e.g. 2040=HD Movies, 5045=UHD TV). A release with subcategory 2040 matches
/// allowed parent 2000, since some indexers only return subcategories in responses.
pub fn release_matches_categories(release_cats: &[i32], allowed_cats: &[i32]) -> bool {
    if allowed_cats.is_empty() {
        return true;
    }
    release_cats.iter().any(|release_cat| {
        allowed_cats.iter().any(|allowed| {
            *release_cat == *allowed || newznab_category_parent(*release_cat) == Some(*allowed)
        })
    })
}

/// Map a newznab subcategory to its parent (1000-level) category.
/// Returns `None` if the category is already a parent or outside the standard range.
fn newznab_category_parent(cat: i32) -> Option<i32> {
    // Standard newznab parents: 1000 (Console), 2000 (Movies), 3000 (Audio),
    // 4000 (PC), 5000 (TV), 6000 (XXX), 7000 (Books), 8000 (Other)
    if (1000..9000).contains(&cat) && cat % 1000 != 0 {
        Some(cat / 1000 * 1000)
    } else {
        None
    }
}

/// Resolve category IDs for a given indexer source and media type.
///
/// Resolution order:
/// 1. If the source has explicit category_ids, use those
/// 2. If the source has indexer_category_mappings for the media_type_id, use those
/// 3. Look up handler_type_defaults for (handler_type, media_type_id)
/// 4. Return empty vec (no filtering)
pub async fn resolve_categories_for_source(
    pool: &PgPool,
    source: &watch_repo::IndexerSourceRow,
    media_type_id: Option<Uuid>,
) -> Result<Vec<i32>, String> {
    // 1. Source-level explicit categories
    if let Some(ref cats) = source.category_ids
        && !cats.is_empty()
    {
        return Ok(cats.clone());
    }

    let Some(mt_id) = media_type_id else {
        return Ok(Vec::new());
    };

    // 2. Per-source category mappings for this media type
    let mappings = category_repo::list_category_mappings_by_media_type(pool, source.id, mt_id)
        .await
        .map_err(|e| e.to_string())?;

    if !mappings.is_empty() {
        let ids: Vec<i32> = mappings.iter().map(|m| m.indexer_category_id).collect();
        return Ok(ids);
    }

    // 3. Handler type defaults
    let defaults = handler_type_defaults::get_categories(pool, &source.handler_type, mt_id)
        .await
        .map_err(|e| e.to_string())?;

    if let Some(cat_strings) = defaults {
        let ids: Vec<i32> = cat_strings
            .iter()
            .filter_map(|s| s.parse::<i32>().ok())
            .collect();
        if !ids.is_empty() {
            return Ok(ids);
        }
    }

    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::repositories::watch as watch_repo;
    use crate::test_utils::{test_db_lock, test_pool};
    use serde_json::json;

    #[tokio::test]
    async fn test_context_from_newznab_source() {
        let _lock = test_db_lock().await;
        let pool = test_pool().await;

        let input = watch_repo::IndexerSourceInput {
            name: "Test NZB".to_string(),
            handler_type: "newznab".to_string(),
            handler_config: serde_json::json!({
                "base_url": "https://indexer.example/api",
                "api_key": "secret"
            }),
            category_ids: None,
            media_type_ids: Vec::new(),
            enabled: true,
            priority: 0,
            seed_ratio: None,
            seed_time_minutes: None,
            search_mode: None,
            auto_throttle: None,
            api_limit_daily: None,
            debrid_mode: None,
        };
        let id = watch_repo::create_indexer_source(&pool, input)
            .await
            .expect("create source");

        let ctx = download_url_context_for_indexer_source(&pool, Some(id))
            .await
            .expect("context");

        assert_eq!(ctx.base_url.as_deref(), Some("https://indexer.example/api"));
        assert!(ctx.auth_header.is_none());

        let _ = watch_repo::delete_indexer_source(&pool, id).await;
    }

    #[tokio::test]
    async fn test_context_from_prowlarr_rss_source() {
        let _lock = test_db_lock().await;
        let pool = test_pool().await;

        let input = watch_repo::IndexerSourceInput {
            name: "Prowlarr RSS".to_string(),
            handler_type: "prowlarr_rss".to_string(),
            handler_config: serde_json::json!({
                "base_url": "http://localhost:9696",
                "api_key": "apikey"
            }),
            category_ids: None,
            media_type_ids: Vec::new(),
            enabled: true,
            priority: 0,
            seed_ratio: None,
            seed_time_minutes: None,
            search_mode: None,
            auto_throttle: None,
            api_limit_daily: None,
            debrid_mode: None,
        };
        let id = watch_repo::create_indexer_source(&pool, input)
            .await
            .expect("create source");

        let ctx = download_url_context_for_indexer_source(&pool, Some(id))
            .await
            .expect("context");

        assert_eq!(ctx.base_url.as_deref(), Some("http://localhost:9696"));
        assert_eq!(
            ctx.auth_header.as_ref().map(|(_, v)| v.as_str()),
            Some("apikey")
        );

        let _ = watch_repo::delete_indexer_source(&pool, id).await;
    }

    #[tokio::test]
    async fn test_context_returns_default_when_no_source_id() {
        let _lock = test_db_lock().await;
        let pool = test_pool().await;

        let ctx = download_url_context_for_indexer_source(&pool, None)
            .await
            .expect("context");

        assert!(ctx.base_url.is_none());
        assert!(ctx.auth_header.is_none());
    }

    #[test]
    fn test_extract_indexer_source_id() {
        let id = Uuid::now_v7();
        let payload = json!({ "indexer_source_id": id.to_string() });
        assert_eq!(extract_indexer_source_id(&payload), Some(id));
    }

    #[test]
    fn test_extract_indexer_source_id_invalid() {
        let payload = json!({ "indexer_source_id": "not-a-uuid" });
        assert_eq!(extract_indexer_source_id(&payload), None);
    }

    #[test]
    fn test_release_matches_categories_exact() {
        assert!(release_matches_categories(&[2000], &[2000]));
        assert!(release_matches_categories(&[5000], &[5000]));
        assert!(!release_matches_categories(&[2000], &[5000]));
    }

    #[test]
    fn test_release_matches_categories_empty_allowed() {
        assert!(release_matches_categories(&[2040], &[]));
        assert!(release_matches_categories(&[], &[]));
    }

    #[test]
    fn test_release_matches_categories_subcategory_matches_parent() {
        // Movie subcategories should match parent 2000
        assert!(release_matches_categories(&[2040], &[2000])); // HD Movies
        assert!(release_matches_categories(&[2045], &[2000])); // UHD Movies
        assert!(release_matches_categories(&[2030], &[2000])); // SD Movies
        assert!(release_matches_categories(&[2010], &[2000])); // Foreign Movies
        assert!(release_matches_categories(&[2050], &[2000])); // 3D Movies
        assert!(release_matches_categories(&[2060], &[2000])); // BluRay

        // TV subcategories should match parent 5000
        assert!(release_matches_categories(&[5040], &[5000])); // HD TV
        assert!(release_matches_categories(&[5045], &[5000])); // UHD TV
        assert!(release_matches_categories(&[5030], &[5000])); // SD TV
        assert!(release_matches_categories(&[5010], &[5000])); // Foreign TV
        assert!(release_matches_categories(&[5080], &[5000])); // Documentary TV

        // Audio subcategories should match parent 3000
        assert!(release_matches_categories(&[3010], &[3000])); // MP3
        assert!(release_matches_categories(&[3040], &[3000])); // Lossless

        // Books subcategories should match parent 7000
        assert!(release_matches_categories(&[7020], &[7000])); // Ebook
        assert!(release_matches_categories(&[7030], &[7000])); // Comics

        // Cross-category should NOT match
        assert!(!release_matches_categories(&[5040], &[2000])); // TV HD != Movies
        assert!(!release_matches_categories(&[3010], &[2000])); // Audio != Movies
        assert!(!release_matches_categories(&[7020], &[5000])); // Books != TV
    }

    #[test]
    fn test_release_matches_categories_multiple_release_cats() {
        // Release with both parent and sub should match
        assert!(release_matches_categories(&[2000, 2040], &[2000]));
        // Release with only sub should still match parent
        assert!(release_matches_categories(&[2040], &[2000]));
        // Release with multiple subs from same parent
        assert!(release_matches_categories(&[2040, 2045], &[2000]));
    }

    #[test]
    fn test_release_matches_categories_multiple_allowed() {
        // Movies + TV allowed
        assert!(release_matches_categories(&[2040], &[2000, 5000]));
        assert!(release_matches_categories(&[5040], &[2000, 5000]));
        assert!(!release_matches_categories(&[3010], &[2000, 5000]));
    }
}
