//! Prowlarr → native indexer source migration.
//!
//! Reads indexer configurations from a Prowlarr instance (including credentials
//! from the `fields` array) and creates native Kobload indexer sources instead
//! of `prowlarr_rss` wrappers.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::info;
use uuid::Uuid;

use crate::prowlarr::{ProwlarrClient, ProwlarrIndexer, ProwlarrIndexerField};
use crate::repositories::clients as clients_repo;
use crate::repositories::watch as watch_repo;

// =============================================================================
// Types
// =============================================================================

#[derive(Debug, Serialize)]
pub struct MigrationPreview {
    pub indexers: Vec<IndexerMigrationItem>,
    pub download_clients: Vec<DownloadClientMatch>,
}

#[derive(Debug, Serialize, Clone)]
pub struct IndexerMigrationItem {
    pub prowlarr_id: i32,
    pub name: String,
    pub handler_type: String,
    pub handler_config: serde_json::Value,
    pub category_ids: Vec<i32>,
    pub protocol: String,
    pub supports_rss: bool,
    pub supports_search: bool,
    pub status: MigrationItemStatus,
    pub existing_source_id: Option<Uuid>,
    pub existing_source_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MigrationItemStatus {
    WillCreate,
    AlreadyExists,
    Unsupported { reason: String },
    MissingFields { fields: Vec<String> },
}

#[derive(Debug, Serialize)]
pub struct DownloadClientMatch {
    pub prowlarr_name: String,
    pub prowlarr_implementation: String,
    pub prowlarr_protocol: String,
    pub kobload_client_id: Option<Uuid>,
    pub kobload_client_name: Option<String>,
    pub match_status: DownloadClientMatchStatus,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadClientMatchStatus {
    Matched,
    NoMatch,
}

#[derive(Debug, Deserialize)]
pub struct MigrationConfirmItem {
    pub prowlarr_id: i32,
    pub name: String,
    pub handler_type: String,
    pub handler_config: serde_json::Value,
    pub category_ids: Vec<i32>,
}

#[derive(Debug, Serialize)]
pub struct MigrationResult {
    pub created: usize,
    pub errors: Vec<String>,
}

// =============================================================================
// Field Extraction
// =============================================================================

fn get_field_str(fields: &[ProwlarrIndexerField], name: &str) -> Option<String> {
    fields
        .iter()
        .find(|f| f.name == name)
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match v {
            serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
            _ => None,
        })
}

// =============================================================================
// Implementation → Handler Type Mapping
// =============================================================================

fn map_implementation(implementation: Option<&str>) -> Option<&'static str> {
    match implementation? {
        "Newznab" => Some("newznab"),
        "Torznab" => Some("torznab"),
        "Cardigann" => Some("cardigann"),
        "MyAnonamouse" => Some("mam"),
        "TorrentDay" => Some("torrentday"),
        _ => None,
    }
}

// =============================================================================
// Handler Config Builder
// =============================================================================

fn build_handler_config(
    indexer: &ProwlarrIndexer,
    handler_type: &str,
) -> Result<serde_json::Value, Vec<String>> {
    match handler_type {
        "newznab" | "torznab" => build_newznab_config(&indexer.fields),
        "cardigann" => build_cardigann_config(indexer),
        "mam" => build_mam_config(&indexer.fields),
        "torrentday" => build_torrentday_config(&indexer.fields),
        _ => Err(vec![format!("Unknown handler type: {handler_type}")]),
    }
}

fn build_newznab_config(fields: &[ProwlarrIndexerField]) -> Result<serde_json::Value, Vec<String>> {
    let base_url = get_field_str(fields, "baseUrl");
    if base_url.is_none() {
        return Err(vec!["baseUrl".to_string()]);
    }
    let mut config = serde_json::json!({
        "base_url": base_url.unwrap(),
    });
    if let Some(api_key) = get_field_str(fields, "apiKey") {
        config["api_key"] = serde_json::json!(api_key);
    }
    Ok(config)
}

fn build_cardigann_config(indexer: &ProwlarrIndexer) -> Result<serde_json::Value, Vec<String>> {
    let definition = indexer.definition_name.as_deref().filter(|s| !s.is_empty());
    if definition.is_none() {
        return Err(vec!["definitionName".to_string()]);
    }
    let mut settings = serde_json::Map::new();
    // Copy all non-empty string fields as settings (username, password, cookie, etc.)
    for field in &indexer.fields {
        if let Some(serde_json::Value::String(val)) = &field.value
            && !val.is_empty()
            && field.name != "baseUrl"
            && field.name != "definitionName"
        {
            settings.insert(field.name.clone(), serde_json::json!(val));
        }
    }
    Ok(serde_json::json!({
        "definition": definition.unwrap(),
        "settings": settings,
    }))
}

fn build_mam_config(fields: &[ProwlarrIndexerField]) -> Result<serde_json::Value, Vec<String>> {
    // Prowlarr stores MAM auth as a cookie; extract mam_id from it
    let cookie = get_field_str(fields, "cookie")
        .or_else(|| get_field_str(fields, "mamId"))
        .or_else(|| get_field_str(fields, "mam_id"));

    let mam_id = cookie.as_deref().and_then(extract_mam_id);
    if mam_id.is_none() {
        return Err(vec!["cookie/mam_id".to_string()]);
    }
    Ok(serde_json::json!({
        "mam_id": mam_id.unwrap(),
    }))
}

/// Extract mam_id from a cookie string like "mam_id=abc123; ..." or a bare value.
fn extract_mam_id(cookie: &str) -> Option<String> {
    // If it contains "mam_id=", extract the value
    if let Some(rest) = cookie
        .split(';')
        .find(|part| part.trim().starts_with("mam_id="))
    {
        let val = rest.trim().strip_prefix("mam_id=")?.trim();
        if !val.is_empty() {
            return Some(val.to_string());
        }
    }
    // Otherwise treat the whole string as the mam_id (bare value)
    let trimmed = cookie.trim();
    if !trimmed.is_empty() && !trimmed.contains('=') {
        return Some(trimmed.to_string());
    }
    // Might be a full cookie string without mam_id
    None
}

fn build_torrentday_config(
    fields: &[ProwlarrIndexerField],
) -> Result<serde_json::Value, Vec<String>> {
    let cookie = get_field_str(fields, "cookie");
    if cookie.is_none() {
        return Err(vec!["cookie".to_string()]);
    }
    let mut config = serde_json::json!({
        "cookie": cookie.unwrap(),
        "freeleech_only": false,
    });
    if let Some(base_url) = get_field_str(fields, "baseUrl") {
        config["base_url"] = serde_json::json!(base_url);
    }
    Ok(config)
}

// =============================================================================
// Dedup Checking
// =============================================================================

fn normalize_url(url: &str) -> String {
    url.trim_end_matches('/').to_lowercase()
}

fn find_existing_match(
    handler_type: &str,
    handler_config: &serde_json::Value,
    existing: &[watch_repo::IndexerSourceRow],
) -> Option<(Uuid, String)> {
    for source in existing {
        if !matches_handler_type(handler_type, &source.handler_type) {
            continue;
        }
        if configs_match(handler_type, handler_config, &source.handler_config) {
            return Some((source.id, source.name.clone()));
        }
    }
    None
}

fn matches_handler_type(new_type: &str, existing_type: &str) -> bool {
    if new_type == existing_type {
        return true;
    }
    // newznab and torznab are interchangeable for dedup purposes
    matches!(
        (new_type, existing_type),
        ("newznab", "torznab") | ("torznab", "newznab")
    )
}

fn configs_match(
    handler_type: &str,
    new_config: &serde_json::Value,
    existing_config: &serde_json::Value,
) -> bool {
    match handler_type {
        "newznab" | "torznab" => {
            let new_url = new_config["base_url"].as_str().map(normalize_url);
            let existing_url = existing_config["base_url"].as_str().map(normalize_url);
            let new_key = new_config["api_key"].as_str();
            let existing_key = existing_config["api_key"].as_str();
            new_url == existing_url && new_key == existing_key
        }
        "cardigann" => {
            let new_def = new_config["definition"].as_str();
            let existing_def = existing_config["definition"].as_str();
            new_def.is_some() && new_def == existing_def
        }
        "mam" => {
            let new_id = new_config["mam_id"].as_str();
            let existing_id = existing_config["mam_id"].as_str();
            new_id.is_some() && new_id == existing_id
        }
        "torrentday" => {
            let new_cookie = new_config["cookie"].as_str();
            let existing_cookie = existing_config["cookie"].as_str();
            new_cookie.is_some() && new_cookie == existing_cookie
        }
        _ => false,
    }
}

// =============================================================================
// Download Client Matching
// =============================================================================

fn map_prowlarr_client_type(implementation: &str) -> Option<&'static str> {
    match implementation {
        "Transmission" => Some("transmission"),
        "SABnzbd" | "Sabnzbd" => Some("sabnzbd"),
        _ => None,
    }
}

// =============================================================================
// Mask Sensitive Fields
// =============================================================================

fn mask_config(handler_type: &str, config: &serde_json::Value) -> serde_json::Value {
    let mut masked = config.clone();
    let sensitive = match handler_type {
        "newznab" | "torznab" => vec!["api_key"],
        "mam" => vec!["mam_id"],
        "torrentday" => vec!["cookie"],
        "cardigann" => vec!["password", "cookie", "apikey"],
        _ => vec![],
    };

    if let Some(obj) = masked.as_object_mut() {
        for key in &sensitive {
            if let Some(val) = obj.get(*key)
                && let Some(s) = val.as_str()
                && s.len() > 4
            {
                let visible = &s[..4];
                obj.insert(key.to_string(), serde_json::json!(format!("{visible}****")));
            }
        }
        // Also mask settings sub-object for cardigann
        if handler_type == "cardigann"
            && let Some(serde_json::Value::Object(settings)) = obj.get_mut("settings")
        {
            for key in ["password", "cookie", "apikey", "passkey"] {
                if let Some(serde_json::Value::String(s)) = settings.get(key)
                    && s.len() > 4
                {
                    let visible = &s[..4];
                    settings.insert(key.to_string(), serde_json::json!(format!("{visible}****")));
                }
            }
        }
    }
    masked
}

// =============================================================================
// Debrid Mode Detection
// =============================================================================

/// Known public tracker names (case-insensitive substring match).
const PUBLIC_TRACKER_NAMES: &[&str] = &[
    "1337x",
    "yts",
    "yify",
    "eztv",
    "thepiratebay",
    "tpb",
    "torrentgalaxy",
    "kickasstorrents",
    "kat",
    "rarbg",
    "nyaa",
    "rutracker",
];

/// Detect the appropriate debrid_mode for a Prowlarr indexer.
///
/// Returns `Some("always")` for public torrent indexers, `None` (use default) for others.
fn detect_debrid_mode(indexer: &ProwlarrIndexer) -> Option<String> {
    // Only relevant for torrent indexers
    if indexer.protocol != "torrent" {
        return None;
    }

    // Check privacy field from Prowlarr API
    if let Some(ref privacy) = indexer.privacy {
        let privacy_lower = privacy.to_lowercase();
        if privacy_lower == "public" {
            return Some("always".to_string());
        }
        // Explicit private/semi-private — keep default
        if privacy_lower == "private" || privacy_lower == "semiprivate" {
            return None;
        }
    }

    // Fall back to name matching for known public trackers
    let name_lower = indexer.name.to_lowercase();
    for tracker in PUBLIC_TRACKER_NAMES {
        if name_lower.contains(tracker) {
            return Some("always".to_string());
        }
    }

    None
}

// =============================================================================
// Preview
// =============================================================================

pub async fn preview_prowlarr_migration(
    pool: &PgPool,
    http_client: &reqwest::Client,
    prowlarr_source_id: Uuid,
) -> Result<MigrationPreview, String> {
    // Load the Prowlarr source to get connection details
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

    let client = ProwlarrClient::new_with_client(base_url, api_key, http_client.clone());

    // Fetch indexers with fields
    let indexers = client
        .list_indexers()
        .await
        .map_err(|e| format!("Failed to fetch Prowlarr indexers: {e}"))?;

    // Load existing indexer sources for dedup
    let existing = watch_repo::list_indexer_sources(pool)
        .await
        .map_err(|e| e.to_string())?;

    // Build preview items
    let mut items = Vec::new();
    for indexer in &indexers {
        if !indexer.enable {
            continue;
        }
        items.push(build_preview_item(indexer, &existing));
    }

    // Match download clients
    let download_clients = match client.download_clients().await {
        Ok(prowlarr_clients) => {
            let kobload_clients = clients_repo::list_clients(pool).await.unwrap_or_default();
            build_download_client_matches(&prowlarr_clients, &kobload_clients)
        }
        Err(e) => {
            info!(error = %e, "Could not fetch Prowlarr download clients (non-fatal)");
            Vec::new()
        }
    };

    info!(
        indexer_count = items.len(),
        will_create = items
            .iter()
            .filter(|i| matches!(i.status, MigrationItemStatus::WillCreate))
            .count(),
        already_exists = items
            .iter()
            .filter(|i| matches!(i.status, MigrationItemStatus::AlreadyExists))
            .count(),
        "Prowlarr migration preview built"
    );

    Ok(MigrationPreview {
        indexers: items,
        download_clients,
    })
}

fn build_preview_item(
    indexer: &ProwlarrIndexer,
    existing: &[watch_repo::IndexerSourceRow],
) -> IndexerMigrationItem {
    let handler_type = match map_implementation(indexer.implementation.as_deref()) {
        Some(ht) => ht,
        None => {
            return IndexerMigrationItem {
                prowlarr_id: indexer.id,
                name: indexer.name.clone(),
                handler_type: indexer
                    .implementation
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                handler_config: serde_json::Value::Null,
                category_ids: indexer.categories.iter().map(|c| c.id).collect(),
                protocol: indexer.protocol.clone(),
                supports_rss: indexer.supports_rss,
                supports_search: indexer.supports_search,
                status: MigrationItemStatus::Unsupported {
                    reason: format!(
                        "Unsupported indexer type: {}",
                        indexer.implementation.as_deref().unwrap_or("unknown")
                    ),
                },
                existing_source_id: None,
                existing_source_name: None,
            };
        }
    };

    let config_result = build_handler_config(indexer, handler_type);
    let category_ids: Vec<i32> = indexer.categories.iter().map(|c| c.id).collect();

    match config_result {
        Ok(config) => {
            let (status, existing_id, existing_name) =
                if let Some((id, name)) = find_existing_match(handler_type, &config, existing) {
                    (MigrationItemStatus::AlreadyExists, Some(id), Some(name))
                } else {
                    (MigrationItemStatus::WillCreate, None, None)
                };

            IndexerMigrationItem {
                prowlarr_id: indexer.id,
                name: indexer.name.clone(),
                handler_type: handler_type.to_string(),
                handler_config: mask_config(handler_type, &config),
                category_ids,
                protocol: indexer.protocol.clone(),
                supports_rss: indexer.supports_rss,
                supports_search: indexer.supports_search,
                status,
                existing_source_id: existing_id,
                existing_source_name: existing_name,
            }
        }
        Err(missing_fields) => IndexerMigrationItem {
            prowlarr_id: indexer.id,
            name: indexer.name.clone(),
            handler_type: handler_type.to_string(),
            handler_config: serde_json::Value::Null,
            category_ids,
            protocol: indexer.protocol.clone(),
            supports_rss: indexer.supports_rss,
            supports_search: indexer.supports_search,
            status: MigrationItemStatus::MissingFields {
                fields: missing_fields,
            },
            existing_source_id: None,
            existing_source_name: None,
        },
    }
}

fn build_download_client_matches(
    prowlarr_clients: &[crate::prowlarr::DownloadClient],
    kobload_clients: &[crate::types::DownloadClientConfig],
) -> Vec<DownloadClientMatch> {
    prowlarr_clients
        .iter()
        .filter(|c| c.enable)
        .map(|pc| {
            let impl_str = pc.implementation.as_deref().unwrap_or("Unknown");
            let protocol = pc.protocol.as_deref().unwrap_or("unknown");
            let mapped_type = map_prowlarr_client_type(impl_str);

            // Try to find a matching kobload client
            let matched = mapped_type.and_then(|client_type| {
                // First try matching by type + base_url
                let prowlarr_url = get_field_str(&pc.fields, "host")
                    .or_else(|| get_field_str(&pc.fields, "urlBase"))
                    .map(|u| normalize_url(&u));

                kobload_clients
                    .iter()
                    .find(|kc| {
                        kc.client_type == client_type
                            && kc.protocol == protocol
                            && (prowlarr_url.is_none()
                                || prowlarr_url.as_deref() == Some(&normalize_url(&kc.base_url)))
                    })
                    .or_else(|| {
                        // Fall back to just type + protocol match
                        kobload_clients
                            .iter()
                            .find(|kc| kc.client_type == client_type && kc.protocol == protocol)
                    })
            });

            DownloadClientMatch {
                prowlarr_name: pc.name.clone().unwrap_or_else(|| impl_str.to_string()),
                prowlarr_implementation: impl_str.to_string(),
                prowlarr_protocol: protocol.to_string(),
                kobload_client_id: matched.map(|m| m.id),
                kobload_client_name: matched.map(|m| m.name.clone()),
                match_status: if matched.is_some() {
                    DownloadClientMatchStatus::Matched
                } else {
                    DownloadClientMatchStatus::NoMatch
                },
            }
        })
        .collect()
}

// =============================================================================
// Confirm
// =============================================================================

pub async fn confirm_prowlarr_migration(
    pool: &PgPool,
    http_client: &reqwest::Client,
    prowlarr_source_id: Uuid,
    items: Vec<MigrationConfirmItem>,
) -> Result<MigrationResult, String> {
    // Re-fetch from Prowlarr to get un-masked configs
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

    let client = ProwlarrClient::new_with_client(base_url, api_key, http_client.clone());
    let indexers = client
        .list_indexers()
        .await
        .map_err(|e| format!("Failed to fetch Prowlarr indexers: {e}"))?;

    // Build a map of prowlarr_id -> indexer for fresh credential lookup
    let indexer_map: std::collections::HashMap<i32, &ProwlarrIndexer> =
        indexers.iter().map(|i| (i.id, i)).collect();

    let mut created = 0usize;
    let mut errors = Vec::new();

    for item in &items {
        // Look up the fresh indexer from Prowlarr to get un-masked config
        let fresh_config = if let Some(prowlarr_indexer) = indexer_map.get(&item.prowlarr_id) {
            match build_handler_config(prowlarr_indexer, &item.handler_type) {
                Ok(config) => config,
                Err(missing) => {
                    errors.push(format!(
                        "{}: missing fields: {}",
                        item.name,
                        missing.join(", ")
                    ));
                    continue;
                }
            }
        } else {
            errors.push(format!(
                "{}: not found in Prowlarr (id={})",
                item.name, item.prowlarr_id
            ));
            continue;
        };

        // Detect public torrent indexers and default debrid_mode to "always"
        let debrid_mode = if let Some(prowlarr_indexer) = indexer_map.get(&item.prowlarr_id) {
            detect_debrid_mode(prowlarr_indexer)
        } else {
            None
        };

        let input = watch_repo::IndexerSourceInput {
            name: item.name.clone(),
            handler_type: item.handler_type.clone(),
            handler_config: fresh_config,
            category_ids: if item.category_ids.is_empty() {
                None
            } else {
                Some(item.category_ids.clone())
            },
            media_type_ids: Vec::new(),
            enabled: true,
            priority: 0,
            seed_ratio: None,
            seed_time_minutes: None,
            search_mode: None,
            auto_throttle: None,
            api_limit_daily: None,
            debrid_mode,
        };

        match watch_repo::create_indexer_source(pool, input).await {
            Ok(id) => {
                info!(
                    source_id = %id,
                    name = %item.name,
                    handler_type = %item.handler_type,
                    "Created native indexer source from Prowlarr migration"
                );
                created += 1;
            }
            Err(e) => {
                errors.push(format!("{}: {}", item.name, e));
            }
        }
    }

    info!(
        created,
        errors = errors.len(),
        "Prowlarr migration complete"
    );

    Ok(MigrationResult { created, errors })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_mam_id_from_cookie() {
        assert_eq!(
            extract_mam_id("mam_id=abc123; other=foo"),
            Some("abc123".to_string())
        );
        assert_eq!(extract_mam_id("mam_id=abc123"), Some("abc123".to_string()));
        assert_eq!(extract_mam_id("abc123"), Some("abc123".to_string()));
        assert_eq!(extract_mam_id(""), None);
        assert_eq!(extract_mam_id("other=foo; bar=baz"), None);
    }

    #[test]
    fn test_map_implementation() {
        assert_eq!(map_implementation(Some("Newznab")), Some("newznab"));
        assert_eq!(map_implementation(Some("Torznab")), Some("torznab"));
        assert_eq!(map_implementation(Some("Cardigann")), Some("cardigann"));
        assert_eq!(map_implementation(Some("MyAnonamouse")), Some("mam"));
        assert_eq!(map_implementation(Some("TorrentDay")), Some("torrentday"));
        assert_eq!(map_implementation(Some("FooBar")), None);
        assert_eq!(map_implementation(None), None);
    }

    #[test]
    fn test_build_newznab_config() {
        let fields = vec![
            ProwlarrIndexerField {
                name: "baseUrl".to_string(),
                value: Some(serde_json::json!("https://api.nzbgeek.info")),
            },
            ProwlarrIndexerField {
                name: "apiKey".to_string(),
                value: Some(serde_json::json!("mykey123")),
            },
        ];
        let config = build_newznab_config(&fields).unwrap();
        assert_eq!(config["base_url"], "https://api.nzbgeek.info");
        assert_eq!(config["api_key"], "mykey123");
    }

    #[test]
    fn test_build_newznab_config_missing_url() {
        let fields = vec![ProwlarrIndexerField {
            name: "apiKey".to_string(),
            value: Some(serde_json::json!("mykey123")),
        }];
        let err = build_newznab_config(&fields).unwrap_err();
        assert_eq!(err, vec!["baseUrl"]);
    }

    #[test]
    fn test_configs_match_newznab() {
        let a = serde_json::json!({"base_url": "https://example.com/", "api_key": "abc"});
        let b = serde_json::json!({"base_url": "https://example.com", "api_key": "abc"});
        assert!(configs_match("newznab", &a, &b));

        let c = serde_json::json!({"base_url": "https://example.com", "api_key": "xyz"});
        assert!(!configs_match("newznab", &a, &c));
    }

    #[test]
    fn test_mask_config() {
        let config =
            serde_json::json!({"base_url": "https://example.com", "api_key": "mysecretkey123"});
        let masked = mask_config("newznab", &config);
        assert_eq!(masked["base_url"], "https://example.com");
        assert_eq!(masked["api_key"], "myse****");
    }

    fn make_indexer(name: &str, protocol: &str, privacy: Option<&str>) -> ProwlarrIndexer {
        ProwlarrIndexer {
            id: 1,
            name: name.to_string(),
            enable: true,
            protocol: protocol.to_string(),
            priority: 0,
            supports_rss: true,
            supports_search: true,
            implementation: None,
            implementation_name: None,
            categories: vec![],
            fields: vec![],
            definition_name: None,
            privacy: privacy.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_detect_debrid_mode_public_privacy() {
        let idx = make_indexer("SomeTracker", "torrent", Some("public"));
        assert_eq!(detect_debrid_mode(&idx), Some("always".to_string()));
    }

    #[test]
    fn test_detect_debrid_mode_private_privacy() {
        let idx = make_indexer("SomeTracker", "torrent", Some("private"));
        assert_eq!(detect_debrid_mode(&idx), None);
    }

    #[test]
    fn test_detect_debrid_mode_usenet_ignored() {
        let idx = make_indexer("1337x", "usenet", Some("public"));
        assert_eq!(detect_debrid_mode(&idx), None);
    }

    #[test]
    fn test_detect_debrid_mode_name_match() {
        let idx = make_indexer("1337x", "torrent", None);
        assert_eq!(detect_debrid_mode(&idx), Some("always".to_string()));

        let idx = make_indexer("YTS Movies", "torrent", None);
        assert_eq!(detect_debrid_mode(&idx), Some("always".to_string()));

        let idx = make_indexer("EZTV", "torrent", None);
        assert_eq!(detect_debrid_mode(&idx), Some("always".to_string()));

        let idx = make_indexer("Nyaa.si", "torrent", None);
        assert_eq!(detect_debrid_mode(&idx), Some("always".to_string()));
    }

    #[test]
    fn test_detect_debrid_mode_unknown_private() {
        let idx = make_indexer("MyPrivateTracker", "torrent", None);
        assert_eq!(detect_debrid_mode(&idx), None);
    }
}
