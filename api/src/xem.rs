use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashSet;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::repositories::xem_cache as xem_repo;
use crate::repositories::xem_titles as xem_titles_repo;

const HAVEMAP_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const SHOW_MAP_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const TITLE_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const XEM_BASE_URL: &str = "https://thexem.info/map";

// --- Types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XemShowMap {
    entries: Vec<XemEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XemEntry {
    pub scene: XemCoords,
    pub tvdb: XemCoords,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XemCoords {
    pub season: i32,
    pub episode: i32,
    pub absolute: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneMapping {
    pub scene_season: u32,
    pub scene_episode: u32,
}

#[derive(Debug, Clone)]
pub struct XemTitle {
    pub title: String,
    pub season: i32, // -1 = all seasons
}

// --- API response deserialization ---

#[derive(Deserialize)]
struct HavemapResponse {
    result: String,
    data: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct AllMappingsResponse {
    result: String,
    data: Vec<MappingEntry>,
}

#[derive(Deserialize)]
struct AllNamesResponse {
    result: String,
    data: HashMap<String, Vec<String>>,
}

#[derive(Deserialize)]
struct MappingEntry {
    scene: CoordsRaw,
    tvdb: CoordsRaw,
}

#[derive(Deserialize)]
struct CoordsRaw {
    season: i32,
    episode: i32,
    absolute: Option<i32>,
}

// --- XemShowMap methods ---

impl XemShowMap {
    pub fn new(entries: Vec<XemEntry>) -> Self {
        Self { entries }
    }

    /// Forward lookup: given TVDB season/episode, find scene numbering.
    pub fn scene_for_tvdb(&self, season: u32, episode: u32) -> Option<SceneMapping> {
        let s = season as i32;
        let e = episode as i32;
        self.entries
            .iter()
            .find(|entry| entry.tvdb.season == s && entry.tvdb.episode == e)
            .map(|entry| SceneMapping {
                scene_season: entry.scene.season as u32,
                scene_episode: entry.scene.episode as u32,
            })
    }

    /// Season-level forward lookup: given TVDB season, find the scene season number.
    /// Derives from the first episode mapping found in that season.
    pub fn scene_season_for_tvdb(&self, season: u32) -> Option<u32> {
        let s = season as i32;
        self.entries
            .iter()
            .find(|entry| entry.tvdb.season == s)
            .map(|entry| entry.scene.season as u32)
    }

    /// Reverse lookup: given scene season/episode, find TVDB numbering.
    /// Used for collision detection.
    pub fn tvdb_for_scene(&self, season: u32, episode: u32) -> Option<(u32, u32)> {
        let s = season as i32;
        let e = episode as i32;
        self.entries
            .iter()
            .find(|entry| entry.scene.season == s && entry.scene.episode == e)
            .map(|entry| (entry.tvdb.season as u32, entry.tvdb.episode as u32))
    }
}

// --- Cache ---

/// XEM cache backed by Postgres for persistence across restarts.
///
/// Havemap (small list of TVDB IDs) stays in memory.
/// Per-show mappings are stored in the `xem_cache` table.
/// Title aliases are cached in memory (full dataset) and in the `xem_title_cache` table.
type HavemapCache = Arc<RwLock<Option<(Vec<i64>, Instant)>>>;
type TitleCache = Arc<RwLock<Option<(HashMap<i64, Vec<XemTitle>>, Instant)>>>;

pub struct XemCache {
    havemap: HavemapCache,
    title_cache: TitleCache,
    refreshing_havemap: Arc<AtomicBool>,
    refreshing_titles: Arc<AtomicBool>,
    refreshing_shows: Arc<DashSet<i64>>,
}

impl Default for XemCache {
    fn default() -> Self {
        Self::new()
    }
}

impl XemCache {
    pub fn new() -> Self {
        Self {
            havemap: Arc::new(RwLock::new(None)),
            title_cache: Arc::new(RwLock::new(None)),
            refreshing_havemap: Arc::new(AtomicBool::new(false)),
            refreshing_titles: Arc::new(AtomicBool::new(false)),
            refreshing_shows: Arc::new(DashSet::new()),
        }
    }

    /// Load XEM caches on startup. Tries the API first (fresh data), falls
    /// back to DB if XEM is unreachable. Logs but never fails — the service
    /// starts regardless and will retry on first request.
    pub async fn ensure_loaded(&self, client: &Client, pool: &PgPool) {
        // Havemap
        if let Some(ids) = fetch_havemap(client).await {
            let count = ids.len();
            let mut guard = self.havemap.write().await;
            *guard = Some((ids, Instant::now()));
            tracing::info!(count, "XEM havemap loaded from API");
        } else {
            tracing::warn!("XEM havemap API unavailable on startup, falling back to DB");
            match xem_repo::all_tvdb_ids(pool).await {
                Ok(ids) if !ids.is_empty() => {
                    let count = ids.len();
                    let mut guard = self.havemap.write().await;
                    *guard = Some((ids, Instant::now()));
                    tracing::info!(count, "XEM havemap loaded from DB cache");
                }
                Ok(_) => tracing::debug!("XEM havemap: no cached data in DB either"),
                Err(e) => tracing::warn!(error = %e, "Failed to load XEM havemap from DB"),
            }
        }

        // Title cache
        if let Some(map) = fetch_all_names(client).await {
            let count = map.len();
            store_title_cache_to_db(pool, &map).await;
            let mut guard = self.title_cache.write().await;
            *guard = Some((map, Instant::now()));
            tracing::info!(shows = count, "XEM title cache loaded from API");
        } else {
            tracing::warn!("XEM title cache API unavailable on startup, falling back to DB");
            match xem_titles_repo::get_all(pool).await {
                Ok(rows) if !rows.is_empty() => {
                    let mut map: HashMap<i64, Vec<XemTitle>> = HashMap::new();
                    for r in rows {
                        map.entry(r.tvdb_id).or_default().push(XemTitle {
                            title: r.title,
                            season: r.season,
                        });
                    }
                    let count = map.len();
                    let mut guard = self.title_cache.write().await;
                    *guard = Some((map, Instant::now()));
                    tracing::info!(shows = count, "XEM title cache loaded from DB cache");
                }
                Ok(_) => tracing::debug!("XEM title cache: no cached data in DB either"),
                Err(e) => tracing::warn!(error = %e, "Failed to load XEM title cache from DB"),
            }
        }
    }

    /// Check if a TVDB ID has any XEM mapping.
    /// Returns false on any error (graceful degradation).
    pub async fn has_mapping(&self, client: &Client, tvdb_id: i64) -> bool {
        let guard = self.havemap.read().await;
        if let Some((ref ids, fetched_at)) = *guard {
            let has = ids.binary_search(&tvdb_id).is_ok();
            if fetched_at.elapsed() > HAVEMAP_TTL {
                drop(guard);
                self.trigger_havemap_refresh(client.clone());
            }
            return has;
        }
        drop(guard);

        // First call — only one caller fetches; others degrade gracefully
        if self
            .refreshing_havemap
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        let result = fetch_havemap(client).await;
        self.refreshing_havemap.store(false, Ordering::SeqCst);

        match result {
            Some(ids) => {
                let has = ids.binary_search(&tvdb_id).is_ok();
                let mut guard = self.havemap.write().await;
                *guard = Some((ids, Instant::now()));
                has
            }
            None => false,
        }
    }

    /// Get the full show mapping from DB (or XEM API on miss).
    ///
    /// Returns cached (possibly stale) data and triggers a background
    /// refresh when expired. Returns None only if the show has no XEM
    /// mapping or both DB and API are unavailable.
    pub async fn get_show_map(
        &self,
        client: &Client,
        pool: &PgPool,
        tvdb_id: i64,
    ) -> Option<XemShowMap> {
        // Check DB cache
        if let Ok(Some(row)) = xem_repo::get(pool, tvdb_id).await
            && let Ok(map) = serde_json::from_value::<XemShowMap>(row.scene_mappings)
        {
            let age = time::OffsetDateTime::now_utc() - row.fetched_at;
            if age > time::Duration::seconds(SHOW_MAP_TTL.as_secs() as i64) {
                self.trigger_show_refresh(client.clone(), pool.clone(), tvdb_id);
            }
            return Some(map);
        }

        // Not in DB — check havemap first to avoid pointless fetches
        if !self.has_mapping(client, tvdb_id).await {
            return None;
        }

        // First call — only one caller per show fetches; others get None
        if !self.refreshing_shows.insert(tvdb_id) {
            return None;
        }
        let result = fetch_show_map(client, tvdb_id).await;
        self.refreshing_shows.remove(&tvdb_id);

        let map = result?;
        if let Ok(json) = serde_json::to_value(&map) {
            let _ = xem_repo::put(pool, tvdb_id, &json).await;
        }
        Some(map)
    }

    /// Convenience: get scene mapping for a specific episode.
    pub async fn scene_mapping_for_episode(
        &self,
        client: &Client,
        pool: &PgPool,
        tvdb_id: i64,
        season: u32,
        episode: u32,
    ) -> Option<SceneMapping> {
        let map = self.get_show_map(client, pool, tvdb_id).await?;
        map.scene_for_tvdb(season, episode)
    }

    /// Get XEM title aliases for a TVDB ID.
    ///
    /// Fetches the full allNames dataset on first call, then serves from the
    /// in-memory cache. Triggers background refresh when TTL expires.
    /// Falls back to per-show DB cache if the in-memory cache is empty.
    pub async fn get_titles(&self, client: &Client, pool: &PgPool, tvdb_id: i64) -> Vec<XemTitle> {
        // Check in-memory cache first
        {
            let guard = self.title_cache.read().await;
            if let Some((ref map, fetched_at)) = *guard {
                let titles = map.get(&tvdb_id).cloned().unwrap_or_default();
                if fetched_at.elapsed() > TITLE_CACHE_TTL {
                    drop(guard);
                    self.trigger_titles_refresh(client.clone(), pool.clone());
                }
                return titles;
            }
        }

        // In-memory cache empty — try DB cache for this specific show
        if let Ok(rows) = xem_titles_repo::get_titles(pool, tvdb_id).await
            && !rows.is_empty()
        {
            let titles: Vec<XemTitle> = rows
                .into_iter()
                .map(|r| XemTitle {
                    title: r.title,
                    season: r.season,
                })
                .collect();
            // Trigger full fetch in background to populate in-memory cache
            self.trigger_titles_refresh(client.clone(), pool.clone());
            return titles;
        }

        // Nothing cached — only one caller fetches; others get empty
        if self
            .refreshing_titles
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Vec::new();
        }
        let result = fetch_all_names(client).await;
        self.refreshing_titles.store(false, Ordering::SeqCst);

        match result {
            Some(map) => {
                let titles = map.get(&tvdb_id).cloned().unwrap_or_default();
                store_title_cache_to_db(pool, &map).await;
                let mut guard = self.title_cache.write().await;
                *guard = Some((map, Instant::now()));
                titles
            }
            None => Vec::new(),
        }
    }

    fn trigger_titles_refresh(&self, client: Client, pool: PgPool) {
        if self
            .refreshing_titles
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        let title_cache = Arc::clone(&self.title_cache);
        let refreshing = Arc::clone(&self.refreshing_titles);

        tokio::spawn(async move {
            if let Some(map) = fetch_all_names(&client).await {
                store_title_cache_to_db(&pool, &map).await;
                let mut guard = title_cache.write().await;
                *guard = Some((map, Instant::now()));
                debug!("XEM title cache refreshed in background");
            } else {
                warn!("XEM title cache background refresh failed, serving stale data");
                let mut guard = title_cache.write().await;
                if let Some((_, ref mut fetched_at)) = *guard {
                    *fetched_at = Instant::now();
                }
            }
            refreshing.store(false, Ordering::SeqCst);
        });
    }

    fn trigger_havemap_refresh(&self, client: Client) {
        if self
            .refreshing_havemap
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        let havemap = Arc::clone(&self.havemap);
        let refreshing = Arc::clone(&self.refreshing_havemap);

        tokio::spawn(async move {
            if let Some(ids) = fetch_havemap(&client).await {
                let mut guard = havemap.write().await;
                *guard = Some((ids, Instant::now()));
                debug!("XEM havemap refreshed in background");
            } else {
                warn!("XEM havemap background refresh failed, serving stale data");
                let mut guard = havemap.write().await;
                if let Some((_, ref mut fetched_at)) = *guard {
                    *fetched_at = Instant::now();
                }
            }
            refreshing.store(false, Ordering::SeqCst);
        });
    }

    fn trigger_show_refresh(&self, client: Client, pool: PgPool, tvdb_id: i64) {
        if !self.refreshing_shows.insert(tvdb_id) {
            return;
        }

        let refreshing_shows = Arc::clone(&self.refreshing_shows);

        tokio::spawn(async move {
            if let Some(map) = fetch_show_map(&client, tvdb_id).await {
                if let Ok(json) = serde_json::to_value(&map) {
                    let _ = xem_repo::put(&pool, tvdb_id, &json).await;
                }
                debug!(tvdb_id, "XEM show map refreshed in background");
            } else {
                warn!(
                    tvdb_id,
                    "XEM show map background refresh failed, serving stale data"
                );
                let _ = xem_repo::touch(&pool, tvdb_id).await;
            }
            refreshing_shows.remove(&tvdb_id);
        });
    }
}

// --- HTTP fetch functions ---

async fn fetch_havemap(client: &Client) -> Option<Vec<i64>> {
    let url = format!("{}/havemap?origin=tvdb", XEM_BASE_URL);
    debug!("Fetching XEM havemap");

    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        warn!(status = %resp.status(), "XEM havemap request failed");
        return None;
    }

    let body: HavemapResponse = resp.json().await.ok()?;
    if body.result != "success" {
        warn!("XEM havemap response was not success");
        return None;
    }

    let mut ids: Vec<i64> = body
        .data
        .into_iter()
        .filter_map(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .collect();
    ids.sort_unstable();
    ids.dedup();

    debug!(count = ids.len(), "XEM havemap loaded");
    Some(ids)
}

async fn fetch_show_map(client: &Client, tvdb_id: i64) -> Option<XemShowMap> {
    let url = format!("{}/all?id={}&origin=tvdb", XEM_BASE_URL, tvdb_id);
    debug!(tvdb_id, "Fetching XEM show map");

    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        warn!(tvdb_id, status = %resp.status(), "XEM show map request failed");
        return None;
    }

    let body: AllMappingsResponse = resp.json().await.ok()?;
    if body.result != "success" {
        warn!(tvdb_id, "XEM show map response was not success");
        return None;
    }

    let entries: Vec<XemEntry> = body
        .data
        .into_iter()
        .map(|e| XemEntry {
            scene: XemCoords {
                season: e.scene.season,
                episode: e.scene.episode,
                absolute: e.scene.absolute,
            },
            tvdb: XemCoords {
                season: e.tvdb.season,
                episode: e.tvdb.episode,
                absolute: e.tvdb.absolute,
            },
        })
        .collect();

    debug!(tvdb_id, count = entries.len(), "XEM show map loaded");
    Some(XemShowMap::new(entries))
}

async fn fetch_all_names(client: &Client) -> Option<HashMap<i64, Vec<XemTitle>>> {
    let url = format!(
        "{}/allNames?origin=tvdb&defaultNames=1&language=en&seasonNumbers=1",
        XEM_BASE_URL
    );
    debug!("Fetching XEM allNames");

    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        warn!(status = %resp.status(), "XEM allNames request failed");
        return None;
    }

    let body: AllNamesResponse = match resp.json().await {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "Failed to parse XEM allNames response");
            return None;
        }
    };

    if body.result != "success" {
        warn!("XEM allNames response was not success");
        return None;
    }

    let mut map: HashMap<i64, Vec<XemTitle>> = HashMap::new();
    for (id_str, names) in body.data {
        let tvdb_id: i64 = match id_str.parse() {
            Ok(id) => id,
            Err(_) => continue,
        };
        for title in names {
            map.entry(tvdb_id)
                .or_default()
                .push(XemTitle { title, season: -1 });
        }
    }

    debug!(shows = map.len(), "XEM allNames loaded");
    Some(map)
}

async fn store_title_cache_to_db(pool: &PgPool, map: &HashMap<i64, Vec<XemTitle>>) {
    let all: Vec<(i64, String, i32)> = map
        .iter()
        .flat_map(|(&tvdb_id, titles)| {
            titles
                .iter()
                .map(move |t| (tvdb_id, t.title.clone(), t.season))
        })
        .collect();

    if let Err(e) = xem_titles_repo::put_all(pool, &all).await {
        warn!(error = %e, "Failed to store XEM title cache in DB");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_show_map() -> XemShowMap {
        // American Dad-style mapping: TVDB S14E01 = Scene S12E01
        XemShowMap::new(vec![
            XemEntry {
                tvdb: XemCoords {
                    season: 14,
                    episode: 1,
                    absolute: Some(230),
                },
                scene: XemCoords {
                    season: 12,
                    episode: 1,
                    absolute: Some(230),
                },
            },
            XemEntry {
                tvdb: XemCoords {
                    season: 14,
                    episode: 2,
                    absolute: Some(231),
                },
                scene: XemCoords {
                    season: 12,
                    episode: 2,
                    absolute: Some(231),
                },
            },
            XemEntry {
                tvdb: XemCoords {
                    season: 16,
                    episode: 1,
                    absolute: Some(260),
                },
                scene: XemCoords {
                    season: 14,
                    episode: 1,
                    absolute: Some(260),
                },
            },
        ])
    }

    #[test]
    fn scene_for_tvdb_returns_correct_mapping() {
        let map = sample_show_map();
        let result = map.scene_for_tvdb(14, 1).unwrap();
        assert_eq!(result.scene_season, 12);
        assert_eq!(result.scene_episode, 1);
    }

    #[test]
    fn scene_for_tvdb_returns_none_for_unmapped() {
        let map = sample_show_map();
        assert!(map.scene_for_tvdb(99, 1).is_none());
    }

    #[test]
    fn tvdb_for_scene_returns_correct_reverse_mapping() {
        let map = sample_show_map();
        let (s, e) = map.tvdb_for_scene(12, 1).unwrap();
        assert_eq!(s, 14);
        assert_eq!(e, 1);
    }

    #[test]
    fn scene_season_for_tvdb_returns_correct_mapping() {
        let map = sample_show_map();
        assert_eq!(map.scene_season_for_tvdb(14), Some(12));
        assert_eq!(map.scene_season_for_tvdb(16), Some(14));
    }

    #[test]
    fn scene_season_for_tvdb_returns_none_for_unmapped() {
        let map = sample_show_map();
        assert!(map.scene_season_for_tvdb(99).is_none());
    }

    #[test]
    fn tvdb_for_scene_returns_none_for_unmapped() {
        let map = sample_show_map();
        assert!(map.tvdb_for_scene(99, 1).is_none());
    }

    #[test]
    fn collision_scenario_detected() {
        let map = sample_show_map();

        // We want TVDB S14E01 (= Scene S12E01)
        let wanted_scene = map.scene_for_tvdb(14, 1).unwrap();
        assert_eq!(wanted_scene.scene_season, 12);
        assert_eq!(wanted_scene.scene_episode, 1);

        // A release named "S14E01" — check if scene S14E01 maps to a different TVDB episode
        let reverse = map.tvdb_for_scene(14, 1).unwrap();
        // Scene S14E01 maps to TVDB S16E01, which is NOT our episode (S14E01)
        assert_eq!(reverse, (16, 1));
        assert_ne!(reverse, (14, 1)); // Collision detected!
    }

    #[test]
    fn no_collision_when_scene_matches_tvdb() {
        let map = XemShowMap::new(vec![XemEntry {
            tvdb: XemCoords {
                season: 1,
                episode: 1,
                absolute: Some(1),
            },
            scene: XemCoords {
                season: 1,
                episode: 1,
                absolute: Some(1),
            },
        }]);

        let scene = map.scene_for_tvdb(1, 1).unwrap();
        assert_eq!(scene.scene_season, 1);
        assert_eq!(scene.scene_episode, 1);

        let reverse = map.tvdb_for_scene(1, 1).unwrap();
        assert_eq!(reverse, (1, 1));
    }

    #[test]
    fn havemap_response_parsing() {
        let json = r#"{"result":"success","data":[73141,"78857",12345],"message":"ok"}"#;
        let resp: HavemapResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.result, "success");

        let mut ids: Vec<i64> = resp
            .data
            .into_iter()
            .filter_map(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![12345, 73141, 78857]);
    }

    #[test]
    fn all_mappings_response_parsing() {
        let json = r#"{
            "result": "success",
            "data": [
                {
                    "scene": {"season": 10, "episode": 5, "absolute": null},
                    "tvdb": {"season": 12, "episode": 5, "absolute": null}
                }
            ],
            "message": "ok"
        }"#;
        let resp: AllMappingsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.result, "success");
        assert_eq!(resp.data.len(), 1);
        assert_eq!(resp.data[0].scene.season, 10);
        assert_eq!(resp.data[0].tvdb.season, 12);
    }

    #[test]
    fn all_names_response_parsing() {
        // Real XEM API format: data values are string arrays, not {name: season} maps
        let json = r#"{
            "result": "success",
            "data": {
                "72546": ["CSI: Crime Scene Investigation", "CSI"],
                "80644": ["Clannad", "Clannad ~After Story~"]
            },
            "message": "ok"
        }"#;
        let resp: AllNamesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.result, "success");
        assert_eq!(resp.data.len(), 2);

        let mut map: HashMap<i64, Vec<XemTitle>> = HashMap::new();
        for (id_str, names) in resp.data {
            let tvdb_id: i64 = id_str.parse().unwrap();
            for title in names {
                map.entry(tvdb_id)
                    .or_default()
                    .push(XemTitle { title, season: -1 });
            }
        }

        let csi = map.get(&72546).unwrap();
        assert_eq!(csi.len(), 2);
        assert!(
            csi.iter()
                .any(|t| t.title == "CSI: Crime Scene Investigation" && t.season == -1)
        );
        assert!(csi.iter().any(|t| t.title == "CSI" && t.season == -1));

        let clannad = map.get(&80644).unwrap();
        assert_eq!(clannad.len(), 2);
        assert!(
            clannad
                .iter()
                .any(|t| t.title == "Clannad" && t.season == -1)
        );
        assert!(
            clannad
                .iter()
                .any(|t| t.title == "Clannad ~After Story~" && t.season == -1)
        );
    }
}
