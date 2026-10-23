//! Repository for the torrent hash index — local cache of torrent metadata
//! seeded from bulk dumps and enriched from live searches, external APIs, and DHT.

use std::collections::HashMap;

use nats_jobs::ParsedMedia;
use serde::{Deserialize, Serialize};
use sqlx::{Executor, PgPool, Postgres, Row};

use super::Result;

/// Derive (season, episodes) for a torrent-level hash index entry from already-parsed media.
/// Rules:
///
/// * `is_pack = true` → `(Some(season), None)` — NULL episodes means "any episode of season X".
/// * At least one episode parsed, not a pack → `(Some(season), Some(vec![...]))`.
/// * Zero episodes parsed → `(Some(season), None)`.
/// * Non-TV / failed parse → `(None, None)`.
///
/// The caller-facing search rule is `$ep = ANY(episodes) OR episodes IS NULL`,
/// so packs and un-parseable S/E both still match.
pub fn derive_season_episodes_from_media(media: &ParsedMedia) -> (Option<i16>, Option<Vec<i16>>) {
    let ParsedMedia::Tv(tv) = media else {
        return (None, None);
    };
    let season = tv.season.and_then(|s| i16::try_from(s).ok());
    let episodes = if tv.is_pack || tv.episodes.is_empty() {
        None
    } else {
        let list: Vec<i16> = tv
            .episodes
            .iter()
            .filter_map(|e| i16::try_from(*e).ok())
            .collect();
        if list.is_empty() { None } else { Some(list) }
    };
    (season, episodes)
}

/// Like [`derive_season_episodes_from_media`] but parses the title fresh.
/// Uses `parse_release(title, "tv")` — so it works regardless of the caller's
/// media_kind classification, meaning a TV title accidentally parsed as a movie
/// at the caller still gets its S/E extracted here.
pub fn derive_season_episodes(title: &str) -> (Option<i16>, Option<Vec<i16>>) {
    let parsed = crate::parsing::parse_release(title, "tv");
    derive_season_episodes_from_media(&parsed.media)
}

/// A row from the torrent_hash_index table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HashIndexRow {
    pub info_hash: String,
    pub imdb_id: Option<String>,
    pub tmdb_id: Option<String>,
    pub original_title: String,
    pub parsed_title: Option<String>,
    pub year: Option<i16>,
    pub quality: Option<serde_json::Value>,
    pub size: Option<i64>,
    pub magnet_url: Option<String>,
    pub source: String,
    pub seeders: Option<i32>,
    /// Parsed season from original_title. NULL = movie / failed parse / multi-season pack.
    pub season: Option<i16>,
    /// Parsed episodes from original_title. NULL = pack, movie, or unparsed.
    /// A row with season=X, episodes=NULL is treated as "could contain any episode of season X".
    pub episodes: Option<Vec<i16>>,
}

/// Entry for inserting/upserting into the hash index.
#[derive(Debug, Clone)]
pub struct HashIndexEntry {
    pub info_hash: String,
    pub imdb_id: Option<String>,
    pub tmdb_id: Option<String>,
    pub original_title: String,
    pub parsed_title: Option<String>,
    pub year: Option<i16>,
    pub quality: Option<serde_json::Value>,
    pub size: Option<i64>,
    pub magnet_url: Option<String>,
    pub source: String,
    pub seeders: Option<i32>,
    pub season: Option<i16>,
    pub episodes: Option<Vec<i16>>,
}

/// Fetch recent entries since a timestamp (for hash index RSS polling).
/// Returns entries ordered by last_seen ASC, limited to 1000.
pub async fn search_since(pool: &PgPool, since: time::OffsetDateTime) -> Result<Vec<HashIndexRow>> {
    let rows = sqlx::query_as!(
        HashIndexRow,
        r#"SELECT info_hash, imdb_id, tmdb_id, original_title, parsed_title,
                  year, quality, size, magnet_url, source, seeders,
                  season, episodes
           FROM torrent_hash_index
           WHERE last_seen >= $1
           ORDER BY last_seen ASC
           LIMIT 1000"#,
        since,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Search by IMDB ID — parent-row-only variant (no per-file data).
///
/// Three query modes depending on what's supplied:
///
/// * IMDB only (season/episode both `None`) — returns every row for that
///   IMDB id. Used for movies and "everything for show X" listings.
/// * IMDB + season — parent season must match. Any episodes layout passes:
///   single-ep, multi-ep, or pack. A season-level query covers the whole
///   season.
/// * IMDB + season + episode — parent season matches AND parent's `episodes`
///   explicitly contains the requested episode. Bare packs (parent
///   `episodes IS NULL`) are NOT returned — without file-level data we can't
///   prove they contain the wanted episode. Callers that need file-level
///   fallback should use [`search_by_imdb_with_files`].
pub async fn search_by_imdb<'e, E>(
    exec: E,
    imdb_id: &str,
    season: Option<i16>,
    episode: Option<i16>,
) -> Result<Vec<HashIndexRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        HashIndexRow,
        r#"SELECT info_hash, imdb_id, tmdb_id, original_title, parsed_title,
                  year, quality, size, magnet_url, source, seeders,
                  season, episodes
           FROM torrent_hash_index
           WHERE imdb_id = $1
             AND (
               $2::smallint IS NULL
               OR (
                 season = $2
                 AND (
                   $3::smallint IS NULL
                   OR episodes @> ARRAY[$3]::smallint[]
                 )
               )
             )
           ORDER BY size DESC NULLS LAST
           LIMIT 500"#,
        imdb_id,
        season,
        episode,
    )
    .fetch_all(exec)
    .await?;

    Ok(rows)
}

/// Full-text search by title — fallback when no IMDB ID.
pub async fn search_by_title<'e, E>(
    exec: E,
    title: &str,
    year: Option<i16>,
) -> Result<Vec<HashIndexRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = if let Some(y) = year {
        sqlx::query_as!(
            HashIndexRow,
            r#"SELECT info_hash, imdb_id, tmdb_id, original_title, parsed_title,
                      year, quality, size, magnet_url, source, seeders,
                      season, episodes
               FROM torrent_hash_index
               WHERE to_tsvector('english', original_title) @@ plainto_tsquery('english', $1)
                 AND (year IS NULL OR year = $2)
               ORDER BY ts_rank(to_tsvector('english', original_title), plainto_tsquery('english', $1)) DESC
               LIMIT 500"#,
            title,
            y,
        )
        .fetch_all(exec)
        .await?
    } else {
        sqlx::query_as!(
            HashIndexRow,
            r#"SELECT info_hash, imdb_id, tmdb_id, original_title, parsed_title,
                      year, quality, size, magnet_url, source, seeders,
                      season, episodes
               FROM torrent_hash_index
               WHERE to_tsvector('english', original_title) @@ plainto_tsquery('english', $1)
               ORDER BY ts_rank(to_tsvector('english', original_title), plainto_tsquery('english', $1)) DESC
               LIMIT 500"#,
            title,
        )
        .fetch_all(exec)
        .await?
    };

    Ok(rows)
}

/// A hash index row with optional per-file data.
#[derive(Debug, Clone)]
pub struct HashIndexRowWithFiles {
    pub info_hash: String,
    pub imdb_id: Option<String>,
    pub tmdb_id: Option<String>,
    pub original_title: String,
    pub parsed_title: Option<String>,
    pub year: Option<i16>,
    pub quality: Option<serde_json::Value>,
    pub size: Option<i64>,
    pub magnet_url: Option<String>,
    pub source: String,
    pub seeders: Option<i32>,
    pub season: Option<i16>,
    pub episodes: Option<Vec<i16>>,
    pub files: Vec<HashFileEntry>,
}

/// Search by IMDB ID with per-file data attached.
///
/// A row passes on parent match OR via a file inside the torrent. Three
/// modes depending on supplied args:
///
/// * IMDB only (season/episode both `None`) — every row for that IMDB id.
/// * IMDB + season — parent season matches, OR a video file inside is at
///   that season. Packs at the right season pass via the parent branch.
/// * IMDB + season + episode — parent season matches AND parent's `episodes`
///   explicitly contains the wanted episode, OR a video file inside pins
///   the episode (season matches AND file's `episodes` contains it). Bare
///   packs (parent `episodes IS NULL`, no files) are excluded — no proof of
///   containing the wanted episode.
///
/// Dropped: parent at wrong season with no matching file; parent with NULL
/// season and no matching file.
pub async fn search_by_imdb_with_files(
    pool: &PgPool,
    imdb_id: &str,
    season: Option<i16>,
    episode: Option<i16>,
) -> Result<Vec<HashIndexRowWithFiles>> {
    let rows = sqlx::query(
        r#"SELECT h.info_hash, h.imdb_id, h.tmdb_id, h.original_title, h.parsed_title,
                  h.year, h.quality, h.size, h.magnet_url, h.source, h.seeders,
                  h.season AS parent_season, h.episodes AS parent_episodes,
                  f.file_idx, f.filename AS file_filename, f.size AS file_size,
                  f.season AS file_season, f.episodes AS file_episodes
           FROM torrent_hash_index h
           LEFT JOIN torrent_hash_files f ON f.info_hash = h.info_hash
               AND f.media_type = 'video'
               AND ($2::smallint IS NULL OR f.season IS NULL OR f.season = $2)
               AND ($3::smallint IS NULL OR f.episodes IS NULL OR f.episodes @> ARRAY[$3]::smallint[])
           WHERE h.imdb_id = $1
             AND (
               -- IMDB only — no season/episode filtering.
               $2::smallint IS NULL
               -- Parent matches the requested season. For season-only
               -- queries any episodes layout passes (packs too). For
               -- season+episode queries the parent must explicitly list
               -- the wanted episode — bare packs must go through the
               -- file-level fallback below.
               OR (
                 h.season = $2
                 AND (
                   $3::smallint IS NULL
                   OR h.episodes @> ARRAY[$3]::smallint[]
                 )
               )
               -- File-level fallback: recovers rows whose parent season
               -- is wrong or NULL, and surfaces packs where a file pins
               -- the wanted episode.
               OR EXISTS (
                 SELECT 1 FROM torrent_hash_files f2
                 WHERE f2.info_hash = h.info_hash
                   AND f2.media_type = 'video'
                   AND f2.season = $2
                   AND (
                     $3::smallint IS NULL
                     OR f2.episodes @> ARRAY[$3]::smallint[]
                   )
               )
             )
           ORDER BY h.size DESC NULLS LAST, f.file_idx ASC NULLS LAST
           LIMIT 5000"#,
    )
    .bind(imdb_id)
    .bind(season)
    .bind(episode)
    .fetch_all(pool)
    .await?;

    // Group by info_hash — rows are ordered by (hash, file_idx)
    let mut result: Vec<HashIndexRowWithFiles> = Vec::new();
    let mut current_hash: Option<String> = None;

    for row in &rows {
        let hash: String = row.get("info_hash");
        let file_idx: Option<i32> = row.get("file_idx");
        let file_filename: Option<String> = row.get("file_filename");
        let file_size: Option<i64> = row.get("file_size");

        if current_hash.as_deref() != Some(&hash) {
            current_hash = Some(hash.clone());
            result.push(HashIndexRowWithFiles {
                info_hash: hash.clone(),
                imdb_id: row.get("imdb_id"),
                tmdb_id: row.get("tmdb_id"),
                original_title: row.get("original_title"),
                parsed_title: row.get("parsed_title"),
                year: row.get("year"),
                quality: row.get("quality"),
                size: row.get("size"),
                magnet_url: row.get("magnet_url"),
                source: row.get("source"),
                seeders: row.get("seeders"),
                season: row.get("parent_season"),
                episodes: row.get("parent_episodes"),
                files: Vec::new(),
            });
        }

        if let (Some(idx), Some(filename)) = (file_idx, file_filename)
            && let Some(entry) = result.last_mut()
        {
            let file_season: Option<i16> = row.get("file_season");
            let file_episodes: Option<Vec<i16>> = row.get("file_episodes");
            entry.files.push(HashFileEntry {
                info_hash: hash,
                file_idx: idx,
                filename,
                size: file_size,
                season: file_season,
                episodes: file_episodes,
                media_type: "video".to_string(), // already filtered by SQL
            });
        }
    }

    Ok(result)
}

/// Deduplicate entries by info_hash, merging metadata from duplicates.
/// When the same hash appears multiple times, non-None fields from later
/// entries fill in None fields on the first occurrence.
fn dedup_entries(entries: &[HashIndexEntry]) -> Vec<HashIndexEntry> {
    let mut seen: HashMap<&str, usize> = HashMap::with_capacity(entries.len());
    let mut deduped: Vec<HashIndexEntry> = Vec::with_capacity(entries.len());

    for entry in entries {
        if let Some(&idx) = seen.get(entry.info_hash.as_str()) {
            let existing = &mut deduped[idx];
            if existing.imdb_id.is_none() {
                existing.imdb_id.clone_from(&entry.imdb_id);
            }
            if existing.tmdb_id.is_none() {
                existing.tmdb_id.clone_from(&entry.tmdb_id);
            }
            if existing.quality.is_none() {
                existing.quality.clone_from(&entry.quality);
            }
            if existing.size.is_none() {
                existing.size = entry.size;
            }
            if existing.seeders.is_none() {
                existing.seeders = entry.seeders;
            }
            if existing.parsed_title.is_none() {
                existing.parsed_title.clone_from(&entry.parsed_title);
            }
            if existing.magnet_url.is_none() {
                existing.magnet_url.clone_from(&entry.magnet_url);
            }
            if existing.year.is_none() {
                existing.year = entry.year;
            }
            if existing.season.is_none() {
                existing.season = entry.season;
            }
            if existing.episodes.is_none() {
                existing.episodes.clone_from(&entry.episodes);
            }
        } else {
            seen.insert(&entry.info_hash, deduped.len());
            deduped.push(entry.clone());
        }
    }

    deduped
}

/// Bulk insert entries using unnest arrays. Returns number of rows inserted.
/// On conflict: enriches existing rows — fills in null IMDB/TMDB/quality/size
/// from the new data without overwriting existing values.
pub async fn bulk_insert(pool: &PgPool, entries: &[HashIndexEntry]) -> Result<u64> {
    if entries.is_empty() {
        return Ok(0);
    }

    upsert_entries(pool, entries, 2000).await
}

/// Upsert a batch of entries — for incremental updates from search results, APIs, etc.
/// Updates last_seen on conflict, and merges quality/size if they were previously null.
pub async fn upsert_batch(pool: &PgPool, entries: &[HashIndexEntry]) -> Result<u64> {
    if entries.is_empty() {
        return Ok(0);
    }
    upsert_entries(pool, entries, 500).await
}

/// Encode an Option<Vec<i16>> as the Postgres array-literal string form
/// (`"{1,2,3}"`) or NULL. Used for unnest-bulk-insert — Postgres doesn't
/// support ragged 2-D arrays, so we pass one text per row and cast
/// `::smallint[]` per row in the SELECT.
fn episodes_to_pg_literal(episodes: Option<&[i16]>) -> Option<String> {
    let list = episodes?;
    let mut s = String::with_capacity(2 + list.len() * 4);
    s.push('{');
    for (i, e) in list.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&e.to_string());
    }
    s.push('}');
    Some(s)
}

/// Shared insert-with-upsert helper used by `bulk_insert` and `upsert_batch`.
async fn upsert_entries(
    pool: &PgPool,
    entries: &[HashIndexEntry],
    chunk_size: usize,
) -> Result<u64> {
    let entries = dedup_entries(entries);
    let mut total = 0u64;
    for chunk in entries.chunks(chunk_size) {
        let info_hashes: Vec<&str> = chunk.iter().map(|e| e.info_hash.as_str()).collect();
        let imdb_ids: Vec<Option<&str>> = chunk.iter().map(|e| e.imdb_id.as_deref()).collect();
        let tmdb_ids: Vec<Option<&str>> = chunk.iter().map(|e| e.tmdb_id.as_deref()).collect();
        let original_titles: Vec<&str> = chunk.iter().map(|e| e.original_title.as_str()).collect();
        let parsed_titles: Vec<Option<&str>> =
            chunk.iter().map(|e| e.parsed_title.as_deref()).collect();
        let years: Vec<Option<i16>> = chunk.iter().map(|e| e.year).collect();
        let qualities: Vec<Option<serde_json::Value>> =
            chunk.iter().map(|e| e.quality.clone()).collect();
        let sizes: Vec<Option<i64>> = chunk.iter().map(|e| e.size).collect();
        let magnet_urls: Vec<Option<&str>> =
            chunk.iter().map(|e| e.magnet_url.as_deref()).collect();
        let sources: Vec<&str> = chunk.iter().map(|e| e.source.as_str()).collect();
        let seeders: Vec<Option<i32>> = chunk.iter().map(|e| e.seeders).collect();
        let seasons: Vec<Option<i16>> = chunk.iter().map(|e| e.season).collect();
        let episodes_lit: Vec<Option<String>> = chunk
            .iter()
            .map(|e| episodes_to_pg_literal(e.episodes.as_deref()))
            .collect();

        // Note: the `episodes` column is smallint[], so we pass one text per
        // row (array literal or NULL) and cast per-row via `ep_lit::smallint[]`.
        let result = sqlx::query(
            r#"INSERT INTO torrent_hash_index
                   (info_hash, imdb_id, tmdb_id, original_title, parsed_title,
                    year, quality, size, magnet_url, source, seeders,
                    season, episodes)
               SELECT info_hash, imdb_id, tmdb_id, original_title, parsed_title,
                      year, quality, size, magnet_url, source, seeders,
                      season, ep_lit::smallint[]
               FROM unnest(
                   $1::text[], $2::text[], $3::text[], $4::text[], $5::text[],
                   $6::smallint[], $7::jsonb[], $8::bigint[], $9::text[], $10::text[], $11::int[],
                   $12::smallint[], $13::text[]
               ) AS u(info_hash, imdb_id, tmdb_id, original_title, parsed_title,
                      year, quality, size, magnet_url, source, seeders,
                      season, ep_lit)
               ON CONFLICT (info_hash) DO UPDATE SET
                   last_seen = NOW(),
                   imdb_id = COALESCE(torrent_hash_index.imdb_id, EXCLUDED.imdb_id),
                   tmdb_id = COALESCE(torrent_hash_index.tmdb_id, EXCLUDED.tmdb_id),
                   quality = CASE
                       WHEN torrent_hash_index.quality IS NULL OR torrent_hash_index.quality = '{}'::jsonb
                       THEN COALESCE(EXCLUDED.quality, torrent_hash_index.quality)
                       ELSE torrent_hash_index.quality
                   END,
                   original_title = CASE
                       WHEN length(EXCLUDED.original_title) > length(torrent_hash_index.original_title)
                       THEN EXCLUDED.original_title
                       ELSE torrent_hash_index.original_title
                   END,
                   parsed_title = CASE
                       WHEN torrent_hash_index.parsed_title IS NULL
                       THEN EXCLUDED.parsed_title
                       ELSE torrent_hash_index.parsed_title
                   END,
                   year = COALESCE(torrent_hash_index.year, EXCLUDED.year),
                   size = COALESCE(torrent_hash_index.size, EXCLUDED.size),
                   seeders = COALESCE(EXCLUDED.seeders, torrent_hash_index.seeders),
                   season = COALESCE(torrent_hash_index.season, EXCLUDED.season),
                   episodes = COALESCE(torrent_hash_index.episodes, EXCLUDED.episodes)"#,
        )
        .bind(&info_hashes as &[&str])
        .bind(&imdb_ids as &[Option<&str>])
        .bind(&tmdb_ids as &[Option<&str>])
        .bind(&original_titles as &[&str])
        .bind(&parsed_titles as &[Option<&str>])
        .bind(&years as &[Option<i16>])
        .bind(&qualities as &[Option<serde_json::Value>])
        .bind(&sizes as &[Option<i64>])
        .bind(&magnet_urls as &[Option<&str>])
        .bind(&sources as &[&str])
        .bind(&seeders as &[Option<i32>])
        .bind(&seasons as &[Option<i16>])
        .bind(&episodes_lit as &[Option<String>])
        .execute(pool)
        .await?;

        total += result.rows_affected();
    }

    Ok(total)
}

/// Upsert a single entry — for RSS polling and one-off indexing.
pub async fn upsert_single(pool: &PgPool, entry: &HashIndexEntry) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO torrent_hash_index
               (info_hash, imdb_id, tmdb_id, original_title, parsed_title,
                year, quality, size, magnet_url, source, seeders,
                season, episodes)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
           ON CONFLICT (info_hash) DO UPDATE SET
               last_seen = NOW(),
               imdb_id = COALESCE(torrent_hash_index.imdb_id, EXCLUDED.imdb_id),
               tmdb_id = COALESCE(torrent_hash_index.tmdb_id, EXCLUDED.tmdb_id),
               quality = CASE
                   WHEN torrent_hash_index.quality IS NULL OR torrent_hash_index.quality = '{}'::jsonb
                   THEN COALESCE(EXCLUDED.quality, torrent_hash_index.quality)
                   ELSE torrent_hash_index.quality
               END,
               original_title = CASE
                   WHEN length(EXCLUDED.original_title) > length(torrent_hash_index.original_title)
                   THEN EXCLUDED.original_title
                   ELSE torrent_hash_index.original_title
               END,
               parsed_title = CASE
                   WHEN torrent_hash_index.parsed_title IS NULL
                   THEN EXCLUDED.parsed_title
                   ELSE torrent_hash_index.parsed_title
               END,
               year = COALESCE(torrent_hash_index.year, EXCLUDED.year),
               size = COALESCE(torrent_hash_index.size, EXCLUDED.size),
               seeders = COALESCE(EXCLUDED.seeders, torrent_hash_index.seeders),
               season = COALESCE(torrent_hash_index.season, EXCLUDED.season),
               episodes = COALESCE(torrent_hash_index.episodes, EXCLUDED.episodes)"#,
        &entry.info_hash,
        entry.imdb_id.as_deref(),
        entry.tmdb_id.as_deref(),
        &entry.original_title,
        entry.parsed_title.as_deref(),
        entry.year,
        entry.quality.as_ref(),
        entry.size,
        entry.magnet_url.as_deref(),
        &entry.source,
        entry.seeders,
        entry.season,
        entry.episodes.as_deref(),
    )
    .execute(pool)
    .await?;

    Ok(())
}

/// Total entry count.
pub async fn count<'e, E>(exec: E) -> Result<i64>
where
    E: Executor<'e, Database = Postgres>,
{
    let count = sqlx::query_scalar!("SELECT COUNT(*) FROM torrent_hash_index")
        .fetch_one(exec)
        .await?;

    Ok(count.unwrap_or(0))
}

/// Entry count grouped by source.
pub async fn count_by_source<'e, E>(exec: E) -> Result<HashMap<String, i64>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query!(
        "SELECT source, COUNT(*) as count FROM torrent_hash_index GROUP BY source ORDER BY count DESC"
    )
    .fetch_all(exec)
    .await?;

    let mut map = HashMap::new();
    for row in rows {
        map.insert(row.source, row.count.unwrap_or(0));
    }
    Ok(map)
}

/// Drop secondary indexes for bulk import performance.
/// The primary key index on info_hash is kept.
pub async fn drop_secondary_indexes(pool: &PgPool) -> std::result::Result<(), sqlx::Error> {
    sqlx::raw_sql(
        "DROP INDEX IF EXISTS idx_hash_index_imdb;
         DROP INDEX IF EXISTS idx_hash_index_tmdb;
         DROP INDEX IF EXISTS idx_hash_index_title;
         DROP INDEX IF EXISTS idx_hash_index_source;
         DROP INDEX IF EXISTS idx_hash_index_imdb_season;
         DROP INDEX IF EXISTS idx_hash_index_episodes_gin;
         ALTER TABLE torrent_hash_index SET (autovacuum_enabled = false);",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// A file entry within a torrent, with parsed metadata.
#[derive(Debug, Clone)]
pub struct HashFileEntry {
    pub info_hash: String,
    pub file_idx: i32,
    pub filename: String,
    pub size: Option<i64>,
    pub season: Option<i16>,
    pub episodes: Option<Vec<i16>>,
    pub media_type: String,
}

/// Classify a torrent file by filename. Thin wrapper around `parsing::classify_filename`.
/// Returns (season, episodes, media_type) where media_type is "video"/"subtitle"/"sample"/"other".
pub fn classify_hash_file(filename: &str) -> (Option<i16>, Option<Vec<i16>>, String) {
    let (season, episodes, media_type) = crate::parsing::classify_filename(filename);
    (season, episodes, media_type.to_string())
}

/// Deduplicate file entries by (info_hash, file_idx). Last-write-wins for duplicates.
fn dedup_file_entries(entries: &[HashFileEntry]) -> Vec<HashFileEntry> {
    let mut seen: HashMap<(&str, i32), usize> = HashMap::with_capacity(entries.len());
    let mut deduped: Vec<HashFileEntry> = Vec::with_capacity(entries.len());

    for entry in entries {
        let key = (entry.info_hash.as_str(), entry.file_idx);
        if let Some(&idx) = seen.get(&key) {
            deduped[idx] = entry.clone();
        } else {
            seen.insert((&entry.info_hash, entry.file_idx), deduped.len());
            deduped.push(entry.clone());
        }
    }

    deduped
}

/// Upsert a batch of file entries into torrent_hash_files.
pub async fn upsert_files(pool: &PgPool, entries: &[HashFileEntry]) -> Result<u64> {
    if entries.is_empty() {
        return Ok(0);
    }

    let entries = dedup_file_entries(entries);
    let mut total = 0u64;
    for chunk in entries.chunks(2000) {
        let info_hashes: Vec<&str> = chunk.iter().map(|e| e.info_hash.as_str()).collect();
        let file_idxs: Vec<i32> = chunk.iter().map(|e| e.file_idx).collect();
        let filenames: Vec<&str> = chunk.iter().map(|e| e.filename.as_str()).collect();
        let sizes: Vec<Option<i64>> = chunk.iter().map(|e| e.size).collect();
        let seasons: Vec<Option<i16>> = chunk.iter().map(|e| e.season).collect();
        let episodes_lit: Vec<Option<String>> = chunk
            .iter()
            .map(|e| episodes_to_pg_literal(e.episodes.as_deref()))
            .collect();
        let media_types: Vec<&str> = chunk.iter().map(|e| e.media_type.as_str()).collect();

        let result = sqlx::query(
            r#"WITH ensure_parents AS (
                   INSERT INTO torrent_hash_index (info_hash, original_title, source)
                   SELECT DISTINCT h, h, 'file_entry'
                   FROM unnest($1::text[]) AS h
                   ON CONFLICT (info_hash) DO NOTHING
               )
               INSERT INTO torrent_hash_files
                   (info_hash, file_idx, filename, size, season, episodes, media_type)
               SELECT info_hash, file_idx, filename, size, season,
                      ep_lit::smallint[], media_type
               FROM unnest(
                   $1::text[], $2::int[], $3::text[], $4::bigint[],
                   $5::smallint[], $6::text[], $7::text[]
               ) AS u(info_hash, file_idx, filename, size, season, ep_lit, media_type)
               ON CONFLICT (info_hash, file_idx) DO UPDATE SET
                   filename = EXCLUDED.filename,
                   size = COALESCE(EXCLUDED.size, torrent_hash_files.size),
                   season = COALESCE(EXCLUDED.season, torrent_hash_files.season),
                   episodes = COALESCE(EXCLUDED.episodes, torrent_hash_files.episodes),
                   media_type = EXCLUDED.media_type"#,
        )
        .bind(&info_hashes as &[&str])
        .bind(&file_idxs as &[i32])
        .bind(&filenames as &[&str])
        .bind(&sizes as &[Option<i64>])
        .bind(&seasons as &[Option<i16>])
        .bind(&episodes_lit as &[Option<String>])
        .bind(&media_types as &[&str])
        .execute(pool)
        .await?;

        total += result.rows_affected();
    }

    Ok(total)
}

/// Look up the filename for a specific file within a torrent by info_hash + file_idx.
pub async fn get_file_name(
    pool: &PgPool,
    info_hash: &str,
    file_idx: i32,
) -> Result<Option<String>> {
    let row = sqlx::query_scalar::<_, String>(
        "SELECT filename FROM torrent_hash_files WHERE info_hash = $1 AND file_idx = $2",
    )
    .bind(info_hash)
    .bind(file_idx)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub async fn get_original_title(pool: &PgPool, info_hash: &str) -> Result<Option<String>> {
    let title = sqlx::query_scalar!(
        "SELECT original_title FROM torrent_hash_index WHERE info_hash = $1",
        info_hash,
    )
    .fetch_optional(pool)
    .await?;
    Ok(title)
}

/// Recreate secondary indexes after bulk import.
pub async fn recreate_secondary_indexes(pool: &PgPool) -> std::result::Result<(), sqlx::Error> {
    sqlx::raw_sql(
        "CREATE INDEX IF NOT EXISTS idx_hash_index_imdb ON torrent_hash_index(imdb_id) WHERE imdb_id IS NOT NULL;
         CREATE INDEX IF NOT EXISTS idx_hash_index_tmdb ON torrent_hash_index(tmdb_id) WHERE tmdb_id IS NOT NULL;
         CREATE INDEX IF NOT EXISTS idx_hash_index_title ON torrent_hash_index USING gin(to_tsvector('english', original_title));
         CREATE INDEX IF NOT EXISTS idx_hash_index_source ON torrent_hash_index(source);
         CREATE INDEX IF NOT EXISTS idx_hash_index_imdb_season ON torrent_hash_index(imdb_id, season) WHERE imdb_id IS NOT NULL;
         CREATE INDEX IF NOT EXISTS idx_hash_index_episodes_gin ON torrent_hash_index USING gin(episodes) WHERE episodes IS NOT NULL;
         ALTER TABLE torrent_hash_index SET (autovacuum_enabled = true);
         ANALYZE torrent_hash_index;",
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct RecentEntry {
    pub info_hash: String,
    pub original_title: String,
    pub size: Option<i64>,
    pub magnet_url: Option<String>,
    pub first_seen: time::OffsetDateTime,
}

/// Entries first seen since `since` whose title has any of `words`, for the
/// review queue. `words` must be plain alphanumeric terms.
pub async fn recent_with_any_word<'e, E>(
    exec: E,
    since: time::OffsetDateTime,
    words: &[String],
    limit: i64,
) -> Result<Vec<RecentEntry>>
where
    E: Executor<'e, Database = Postgres>,
{
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as!(
        RecentEntry,
        r#"
        SELECT info_hash, original_title, size, magnet_url, first_seen
        FROM torrent_hash_index
        WHERE to_tsvector('english', original_title) @@ to_tsquery('english', $1)
          AND first_seen >= $2
        ORDER BY first_seen DESC
        LIMIT $3
        "#,
        words.join(" | "),
        since,
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}
