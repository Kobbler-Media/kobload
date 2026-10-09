use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use reqwest::Client;
use serde::Deserialize;
use sqlx::PgPool;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::repositories::scene_mappings as scene_repo;

const REFRESH_TTL_SECS: i64 = 6 * 60 * 60; // 6 hours
const SONARR_SCENE_URL: &str = "https://services.sonarr.tv/v1/scenemapping";

#[derive(Debug, Clone)]
pub struct SceneTitle {
    pub title: String,
    pub season: i32, // -1 = all seasons
    pub filter_regex: Option<String>,
}

pub struct SceneMappingCache {
    data: Arc<RwLock<HashMap<i64, Vec<SceneTitle>>>>,
    refreshing: Arc<AtomicBool>,
}

#[derive(Deserialize)]
struct SonarrSceneMapping {
    title: String,
    #[serde(rename = "tvdbId")]
    tvdb_id: i64,
    season: i32,
    #[serde(rename = "filterRegex", default)]
    filter_regex: Option<String>,
}

impl Default for SceneMappingCache {
    fn default() -> Self {
        Self::new()
    }
}

impl SceneMappingCache {
    pub fn new() -> Self {
        Self {
            data: Arc::new(RwLock::new(HashMap::new())),
            refreshing: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Load cache from DB on startup. If DB is empty or stale, fetch from API synchronously.
    /// If API fails and DB has data, use stale data. If both fail, log and continue with empty cache.
    pub async fn ensure_loaded(&self, client: &Client, pool: &PgPool) {
        let last_fetched = scene_repo::get_last_fetched(pool).await.ok().flatten();

        if let Some(fetched_at) = last_fetched {
            // DB has data — load it
            match scene_repo::get_all(pool).await {
                Ok(rows) if !rows.is_empty() => {
                    let map = build_map_from_rows(&rows);
                    let count = map.values().map(|v| v.len()).sum::<usize>();
                    {
                        let mut data = self.data.write().await;
                        *data = map;
                    }
                    info!(mappings = count, "Scene mapping cache loaded from DB");

                    let age = time::OffsetDateTime::now_utc() - fetched_at;
                    if age.whole_seconds() > REFRESH_TTL_SECS {
                        self.trigger_refresh(client.clone(), pool.clone());
                    }
                    return;
                }
                Ok(_) => {} // empty rows, fall through
                Err(e) => {
                    warn!(error = %e, "Failed to load scene mappings from DB");
                }
            }
        }

        // No DB data — fetch synchronously from API
        if let Some(map) = fetch_and_store(client, pool).await {
            let count = map.values().map(|v| v.len()).sum::<usize>();
            let mut data = self.data.write().await;
            *data = map;
            info!(mappings = count, "Scene mapping cache loaded from API");
        } else {
            warn!("Scene mapping cache: API unavailable and no DB data, starting with empty cache");
        }
    }

    /// Get scene titles for a TVDB ID (blocking read via try_read, returns empty on contention).
    pub fn get_titles(&self, tvdb_id: i64) -> Vec<SceneTitle> {
        match self.data.try_read() {
            Ok(guard) => guard.get(&tvdb_id).cloned().unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    /// Get scene titles for a TVDB ID (async read).
    pub async fn get_titles_async(&self, tvdb_id: i64) -> Vec<SceneTitle> {
        let guard = self.data.read().await;
        guard.get(&tvdb_id).cloned().unwrap_or_default()
    }

    fn trigger_refresh(&self, client: Client, pool: PgPool) {
        if self
            .refreshing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        let data = Arc::clone(&self.data);
        let refreshing = Arc::clone(&self.refreshing);

        tokio::spawn(async move {
            if let Some(map) = fetch_and_store(&client, &pool).await {
                let count = map.values().map(|v| v.len()).sum::<usize>();
                let mut guard = data.write().await;
                *guard = map;
                debug!(
                    mappings = count,
                    "Scene mapping cache refreshed in background"
                );
            } else {
                warn!("Scene mapping cache background refresh failed, serving stale data");
            }
            refreshing.store(false, Ordering::SeqCst);
        });
    }
}

async fn fetch_and_store(client: &Client, pool: &PgPool) -> Option<HashMap<i64, Vec<SceneTitle>>> {
    debug!("Fetching Sonarr scene mappings");

    let resp = client.get(SONARR_SCENE_URL).send().await.ok()?;
    if !resp.status().is_success() {
        warn!(
            status = %resp.status(),
            "Sonarr scene mapping request failed"
        );
        return None;
    }

    let mappings: Vec<SonarrSceneMapping> = match resp.json().await {
        Ok(m) => m,
        Err(e) => {
            warn!(error = %e, "Failed to parse Sonarr scene mapping response");
            return None;
        }
    };

    info!(count = mappings.len(), "Fetched Sonarr scene mappings");

    // Store in DB
    let db_rows: Vec<(i64, &str, i32, Option<&str>)> = mappings
        .iter()
        .map(|m| {
            (
                m.tvdb_id,
                m.title.as_str(),
                m.season,
                m.filter_regex.as_deref(),
            )
        })
        .collect();

    if let Err(e) = scene_repo::replace_all(pool, &db_rows).await {
        warn!(error = %e, "Failed to store scene mappings in DB");
    }

    if let Err(e) = scene_repo::set_last_fetched(pool).await {
        warn!(error = %e, "Failed to update scene mapping last_fetched");
    }

    Some(build_map(mappings))
}

fn build_map(mappings: Vec<SonarrSceneMapping>) -> HashMap<i64, Vec<SceneTitle>> {
    let mut map: HashMap<i64, Vec<SceneTitle>> = HashMap::new();
    for m in mappings {
        map.entry(m.tvdb_id).or_default().push(SceneTitle {
            title: m.title,
            season: m.season,
            filter_regex: m.filter_regex,
        });
    }
    map
}

fn build_map_from_rows(rows: &[scene_repo::SceneMappingRow]) -> HashMap<i64, Vec<SceneTitle>> {
    let mut map: HashMap<i64, Vec<SceneTitle>> = HashMap::new();
    for row in rows {
        map.entry(row.tvdb_id as i64).or_default().push(SceneTitle {
            title: row.scene_title.clone(),
            season: row.season,
            filter_regex: row.filter_regex.clone(),
        });
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sonarr_response() {
        let json = r#"[{"title":"csi","searchTitle":"csi","tvdbId":72546,"season":-1,"sceneSeasonNumber":null}]"#;
        let mappings: Vec<SonarrSceneMapping> = serde_json::from_str(json).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].title, "csi");
        assert_eq!(mappings[0].tvdb_id, 72546);
        assert_eq!(mappings[0].season, -1);
        assert!(mappings[0].filter_regex.is_none());
    }

    #[test]
    fn test_parse_sonarr_response_with_filter_regex() {
        let json = r#"[{"title":"paradise","searchTitle":"paradise","tvdbId":12345,"season":-1,"filterRegex":"paradise.+no"}]"#;
        let mappings: Vec<SonarrSceneMapping> = serde_json::from_str(json).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].filter_regex.as_deref(), Some("paradise.+no"));
    }

    #[test]
    fn test_build_map_from_response() {
        let mappings = vec![
            SonarrSceneMapping {
                title: "csi".into(),
                tvdb_id: 72546,
                season: -1,
                filter_regex: None,
            },
            SonarrSceneMapping {
                title: "csi miami".into(),
                tvdb_id: 72546,
                season: 2,
                filter_regex: None,
            },
            SonarrSceneMapping {
                title: "other show".into(),
                tvdb_id: 99999,
                season: -1,
                filter_regex: Some("other.+show".into()),
            },
        ];
        let map = build_map(mappings);
        assert_eq!(map.get(&72546).unwrap().len(), 2);
        assert_eq!(map.get(&99999).unwrap().len(), 1);
        assert!(map.get(&99999).unwrap()[0].filter_regex.is_some());
    }
}
