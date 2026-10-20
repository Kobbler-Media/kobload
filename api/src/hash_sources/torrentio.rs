//! Torrentio stream lookup client.
//!
//! Uses `behaviorHints.filename` (the actual release filename) for parsing,
//! with fallback enrichment from the pack name (title line 0) when the filename
//! is bare (missing show/episode or quality info).

use regex::Regex;
use reqwest::Client;
use serde::Deserialize;
use std::sync::LazyLock;
use std::time::Duration;
use tracing::warn;

use crate::parsing::{ParsedMedia, parse_release};
use crate::quality::parse_quality;
use crate::repositories::hash_index::{HashFileEntry, HashIndexEntry};

const BASE_URL: &str = "https://torrentio.strem.fun";
const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Deserialize)]
struct TorrentioResponse {
    #[serde(default)]
    streams: Vec<TorrentioStream>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TorrentioStream {
    /// Provider/quality label, e.g. "[RD+] Torrentio\n1080p". Carries the
    /// debrid cache marker when a debrid key is configured.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    info_hash: Option<String>,
    #[serde(default)]
    file_idx: Option<i32>,
    #[serde(default)]
    behavior_hints: Option<BehaviorHints>,
    /// Pre-authenticated debrid URL, returned instead of `infoHash` when a
    /// debrid key is configured. Embeds the info hash in its path.
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BehaviorHints {
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    binge_group: Option<String>,
}

/// Result from Torrentio: hash index entry + optional file entry.
pub struct TorrentioResult {
    pub entry: HashIndexEntry,
    pub file: Option<HashFileEntry>,
    /// Whether Torrentio reports this as already cached on the configured
    /// debrid provider. `None` when no debrid key was configured, so the
    /// response carried no cache information either way.
    pub debrid_cached: Option<bool>,
    pub binge_group: Option<String>,
}

pub struct TorrentioClient {
    client: Client,
    /// Real-Debrid API key. When set, Torrentio annotates each stream's `name`
    /// with `[RD+]` (cached) or `[RD download]` (not cached), giving us a bulk
    /// cache signal that Real-Debrid itself no longer offers — its
    /// `/torrents/instantAvailability` endpoint now returns error_code 37.
    ///
    /// The status comes from Torrentio's own store (a ~5-day TTL cache
    /// populated by its users), not from a live check against this account, so
    /// treat it as a strong prior rather than as truth.
    ///
    /// IMPORTANT: in debrid mode Torrentio rewrites its response — it drops
    /// `infoHash`/`fileIdx` in favour of a pre-authenticated `url` (parsed back
    /// by `parse_resolve_url`), and on a bad key it returns a single synthetic
    /// "RD error" stream carrying no marker.
    ///
    /// It also returns substantially MORE content than the keyless endpoint,
    /// which is hard-capped at 50 streams — measured 108 vs 50 for a movie and
    /// 94 vs 50 for an episode — so the debrid response is the primary source,
    /// not an overlay.
    real_debrid_key: Option<String>,
}

impl TorrentioClient {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            real_debrid_key: None,
        }
    }

    /// Attach a Real-Debrid key so responses carry per-stream cache status.
    pub fn with_real_debrid_key(mut self, key: Option<String>) -> Self {
        self.real_debrid_key = key.filter(|k| !k.is_empty());
        self
    }

    /// Look up streams for a movie by IMDB ID.
    pub async fn search_movie(&self, imdb_id: &str) -> Vec<TorrentioResult> {
        let path = format!("stream/movie/{imdb_id}.json");
        self.fetch_streams(&path, imdb_id, "movies", None, None)
            .await
    }

    /// Look up streams for a TV episode by IMDB ID, season, and episode.
    pub async fn search_series(
        &self,
        imdb_id: &str,
        season: u32,
        episode: u32,
    ) -> Vec<TorrentioResult> {
        let path = format!("stream/series/{imdb_id}:{season}:{episode}.json");
        self.fetch_streams(&path, imdb_id, "tv", Some(season), Some(episode))
            .await
    }

    /// Fetch the stream list, using the debrid endpoint as the primary source
    /// when a key is configured.
    ///
    /// The two endpoints do not return the same content. The keyless one is
    /// hard-capped at 50 streams; the debrid one returned 108 for the same
    /// movie (94 vs 50 for a TV episode), and the extra results are not
    /// filler — 24 of them were already cached, mostly 4K. Joining markers
    /// onto the keyless list therefore threw away the best instant-play
    /// options, which is the opposite of the point.
    ///
    /// So the debrid response leads, and the keyless one is merged in behind it
    /// to cover the handful of debrid entries whose URL has no parseable file
    /// index (9/108 above) and to keep working when the debrid call fails.
    ///
    /// Both run concurrently: stream search grants Torrentio a 500ms grace
    /// period, and running these back to back would routinely blow it.
    async fn fetch_streams(
        &self,
        path: &str,
        imdb_id: &str,
        media_kind: &str,
        season: Option<u32>,
        episode: Option<u32>,
    ) -> Vec<TorrentioResult> {
        let base_url = format!("{BASE_URL}/{path}");

        let Some(ref key) = self.real_debrid_key else {
            return match self
                .fetch_streams_inner(&base_url, imdb_id, media_kind, season, episode)
                .await
            {
                Ok(entries) => entries,
                Err(e) => {
                    warn!(url = %base_url, error = %e, "torrentio search failed");
                    Vec::new()
                }
            };
        };

        let debrid_url = format!("{BASE_URL}/realdebrid={key}/{path}");
        let (debrid_result, keyless_result) = tokio::join!(
            self.fetch_streams_inner(&debrid_url, imdb_id, media_kind, season, episode),
            self.fetch_streams_inner(&base_url, imdb_id, media_kind, season, episode),
        );

        let mut results = match debrid_result {
            Ok(entries) => entries,
            Err(e) => {
                warn!(url = %debrid_url, error = %e, "torrentio debrid search failed");
                Vec::new()
            }
        };

        // Merge in anything the debrid response could not supply. Debrid entries
        // win on conflict: only they carry cache markers.
        let seen: std::collections::HashSet<String> =
            results.iter().map(|r| r.entry.info_hash.clone()).collect();
        match keyless_result {
            Ok(entries) => {
                for e in entries {
                    if !seen.contains(&e.entry.info_hash) {
                        results.push(e);
                    }
                }
            }
            Err(e) => {
                warn!(url = %base_url, error = %e, "torrentio keyless search failed");
            }
        }

        results
    }

    async fn fetch_streams_inner(
        &self,
        url: &str,
        imdb_id: &str,
        media_kind: &str,
        api_season: Option<u32>,
        api_episode: Option<u32>,
    ) -> Result<Vec<TorrentioResult>, Box<dyn std::error::Error + Send + Sync>> {
        let resp = self
            .client
            .get(url)
            .timeout(TIMEOUT)
            .send()
            .await?
            .error_for_status()?;

        let body = resp.text().await?;
        Ok(parse_streams(
            &body,
            imdb_id,
            media_kind,
            api_season,
            api_episode,
        )?)
    }
}

pub fn parse_streams(
    body: &str,
    imdb_id: &str,
    media_kind: &str,
    api_season: Option<u32>,
    api_episode: Option<u32>,
) -> Result<Vec<TorrentioResult>, serde_json::Error> {
    let data: TorrentioResponse = serde_json::from_str(body)?;

    let results = data
        .streams
        .into_iter()
        .filter_map(|s| {
            // In debrid mode Torrentio omits `infoHash`/`fileIdx` entirely
            // and returns a pre-authenticated resolve URL instead, so fall
            // back to recovering both from that URL's path.
            let resolve = s.url.as_deref().and_then(parse_resolve_url);
            let info_hash = match s.info_hash.as_deref().filter(|h| !h.is_empty()) {
                Some(h) => h.to_lowercase(),
                None => resolve.as_ref()?.info_hash.clone(),
            };
            let file_idx = s
                .file_idx
                .or_else(|| resolve.as_ref().and_then(|r| r.file_idx));

            // Cache status, present only on debrid-mode responses.
            let debrid_cached = s.name.as_deref().and_then(parse_debrid_cached);
            let binge_group = s
                .behavior_hints
                .as_ref()
                .and_then(|h| h.binge_group.clone())
                .filter(|g| !g.is_empty());

            // Parse seeders + size from emoji metadata in title
            let (meta_seeders, meta_size) = s
                .title
                .as_deref()
                .map(parse_title_metadata)
                .unwrap_or((None, None));

            let filename = s
                .behavior_hints
                .as_ref()
                .and_then(|h| h.filename.as_deref())
                .filter(|f| !f.is_empty())
                .map(strip_file_extension);

            let pack_name = s
                .title
                .as_deref()
                .and_then(|t| t.lines().next())
                .filter(|line| !line.is_empty())
                .map(String::from);

            let title = resolve_title(
                filename.as_deref(),
                pack_name.as_deref(),
                media_kind,
                api_season,
                api_episode,
            )?;

            let parsed = parse_release(&title, media_kind);
            let year = match &parsed.media {
                ParsedMedia::Movie(m) => m.year.and_then(|y| i16::try_from(y).ok()),
                _ => None,
            };
            let quality = serde_json::to_value(&parsed.quality).ok();
            // Extract S/E. If the caller classified this as a movie the
            // ParsedMedia isn't Tv, so re-parse as TV to catch S/E markers.
            let (season, episodes) = match &parsed.media {
                ParsedMedia::Tv(_) => {
                    crate::repositories::hash_index::derive_season_episodes_from_media(
                        &parsed.media,
                    )
                }
                _ => crate::repositories::hash_index::derive_season_episodes(&title),
            };

            // Build file entry if we have filename + file_idx
            let file = if let Some(ref hints) = s.behavior_hints
                && let Some(ref raw_filename) = hints.filename
                && !raw_filename.is_empty()
                && let Some(file_idx) = file_idx
            {
                let (season, episodes, file_media_type) =
                    crate::repositories::hash_index::classify_hash_file(raw_filename);
                Some(HashFileEntry {
                    info_hash: info_hash.clone(),
                    file_idx,
                    filename: raw_filename.clone(),
                    size: meta_size,
                    season,
                    episodes,
                    media_type: file_media_type,
                })
            } else {
                None
            };

            Some(TorrentioResult {
                entry: HashIndexEntry {
                    info_hash,
                    imdb_id: Some(imdb_id.to_string()),
                    tmdb_id: None,
                    original_title: title,
                    parsed_title: Some(parsed.title),
                    year,
                    quality,
                    size: meta_size,
                    magnet_url: None,
                    source: "torrentio".to_string(),
                    seeders: meta_seeders,
                    season,
                    episodes,
                },
                file,
                debrid_cached,
                binge_group,
            })
        })
        .collect();

    Ok(results)
}

/// Resolve the best title for a Torrentio stream entry.
///
/// Strategy:
/// 1. Try the filename — if it parses with a show name + season/episode, use it.
/// 2. If the filename is structurally bare (no show or no episode), synthesize
///    a title from the pack name (show + quality) and API season/episode.
/// 3. If the filename is structurally OK but missing quality info (no resolution,
///    no source, no codec), enrich it with quality tokens from the pack name.
/// 4. If there's no filename at all, use the pack name as-is.
fn resolve_title(
    filename: Option<&str>,
    pack_name: Option<&str>,
    media_kind: &str,
    api_season: Option<u32>,
    api_episode: Option<u32>,
) -> Option<String> {
    let Some(filename) = filename else {
        // No filename — use pack name directly
        return pack_name.map(String::from);
    };

    // For movies or when we don't have pack context, just use filename
    if media_kind != "tv" || pack_name.is_none() {
        return Some(filename.to_string());
    }
    let pack_name = pack_name.unwrap();

    // Parse the filename structurally
    let file_parsed = parse_release(filename, "tv");
    let file_is_bare = match &file_parsed.media {
        ParsedMedia::Tv(tv) => tv.season.is_none() && tv.episodes.is_empty(),
        _ => true,
    };

    if file_is_bare {
        if let Some(season) = api_season
            && let Some(episode) = api_episode
        {
            // Synthesize from pack context: extract show name from pack, build
            // S{season}E{episode}, append quality tokens from pack.
            return Some(synthesize_from_pack(pack_name, season, episode));
        }
        // No API season/episode — fall back to pack name as-is
        return Some(pack_name.to_string());
    }

    // Filename parsed OK structurally — check if it needs quality enrichment
    let file_quality = &file_parsed.quality;
    let quality_is_bare = file_quality.resolution.is_none()
        && file_quality.source.is_none()
        && file_quality.codec.is_none();

    if quality_is_bare {
        return Some(enrich_with_pack_quality(filename, pack_name));
    }

    // Filename is fine as-is
    Some(filename.to_string())
}

/// Build a synthetic title from a pack name and known season/episode.
///
/// Extracts the show name by parsing the pack name as TV, then constructs
/// `{show_name} S{season}E{episode} {quality_suffix}` where quality_suffix
/// contains resolution, source, codec, and release group from the pack.
fn synthesize_from_pack(pack_name: &str, season: u32, episode: u32) -> String {
    let pack_parsed = parse_release(pack_name, "tv");
    let show_name = match &pack_parsed.media {
        ParsedMedia::Tv(tv) => tv.show_name.clone(),
        _ => pack_parsed.title.clone(),
    };

    let quality_suffix =
        build_quality_suffix(&pack_parsed.quality, pack_parsed.release_group.as_deref());

    let mut title = format!("{show_name} S{season:02}E{episode:02}");
    if !quality_suffix.is_empty() {
        title.push(' ');
        title.push_str(&quality_suffix);
    }
    title
}

/// Append quality tokens from the pack name to a filename that's missing them.
fn enrich_with_pack_quality(filename: &str, pack_name: &str) -> String {
    let pack_quality = parse_quality(pack_name);
    let suffix = build_quality_suffix(&pack_quality, pack_quality.release_group.as_deref());

    if suffix.is_empty() {
        return filename.to_string();
    }

    format!("{filename} {suffix}")
}

/// Build a dot-separated quality suffix from parsed quality fields.
/// e.g., "1080p.WEB-DL.x265-GroupName" or "HDTV-GCJM"
fn build_quality_suffix(
    quality: &crate::parsing::ParsedQuality,
    release_group: Option<&str>,
) -> String {
    let mut parts: Vec<&str> = Vec::new();

    if let Some(ref res) = quality.resolution {
        parts.push(res);
    }
    if let Some(ref source) = quality.source {
        parts.push(source);
    }
    if let Some(ref codec) = quality.codec {
        parts.push(codec);
    }
    if let Some(ref audio) = quality.audio {
        parts.push(audio);
    }

    let mut result = parts.join(".");
    if let Some(group) = release_group {
        if result.is_empty() {
            result = format!("-{group}");
        } else {
            result = format!("{result}-{group}");
        }
    }
    result
}

static RE_SEEDERS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"👤\s*(\d+)").expect("seeders regex"));
static RE_SIZE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"💾\s*([\d.]+)\s*(GB|MB|KB|TB)").expect("size regex"));

/// Best-effort parse of seeders and file size from torrentio's emoji metadata line.
///
/// The title field contains multiple lines; the last line typically looks like:
/// `👤 3 💾 1.28 GB ⚙️ 1337x`
///
/// Returns `(seeders, size_bytes)` — either or both may be `None` on parse failure.
fn parse_title_metadata(title: &str) -> (Option<i32>, Option<i64>) {
    let seeders = RE_SEEDERS
        .captures(title)
        .and_then(|c| c[1].parse::<i32>().ok());

    let size = RE_SIZE.captures(title).and_then(|c| {
        let value: f64 = c[1].parse().ok()?;
        let bytes = match &c[2] {
            "TB" => value * 1_099_511_627_776.0,
            "GB" => value * 1_073_741_824.0,
            "MB" => value * 1_048_576.0,
            "KB" => value * 1_024.0,
            _ => return None,
        };
        Some(bytes as i64)
    });

    (seeders, size)
}

/// Fields recovered from a Torrentio debrid resolve URL.
pub(crate) struct ResolvedUrl {
    pub info_hash: String,
    pub file_idx: Option<i32>,
}

/// Parse a Torrentio debrid `url` back into the fields the debrid-mode response
/// omits.
///
/// Shape: `/resolve/{moch}/{apiKey}/{infoHash}/{cachedEntryInfo}/{fileIdx}/{filename}`
/// (RD uses the literal string `null` for `cachedEntryInfo`).
///
/// Indexed forward from the `resolve` segment rather than scanned for the first
/// 40-char hex run: the API key sits immediately before the hash and can itself
/// be 40 hex characters, which a naive scan would match instead.
fn parse_resolve_url(url: &str) -> Option<ResolvedUrl> {
    let segments: Vec<&str> = url.split('/').collect();
    let base = segments.iter().position(|s| *s == "resolve")?;

    let hash = segments.get(base + 3)?;
    if hash.len() != 40 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }

    Some(ResolvedUrl {
        info_hash: hash.to_lowercase(),
        file_idx: segments.get(base + 5).and_then(|s| s.parse().ok()),
    })
}

/// Detect Torrentio's debrid cache marker in a stream's `name` field.
///
/// With a debrid key configured, Torrentio prefixes the provider tag:
///   cached           -> "[RD+] Torrentio\n4k DV"
///   not cached       -> "[RD download] Torrentio\n4k HDR"
///   torrent cached,
///   file still needed -> "[RD+ Download] Torrentio\n1080p"
///
/// The third form is why "download" is tested BEFORE the `+` suffix: that tag
/// both ends its service name with `+` and contains "download", and treating it
/// as cached would promote a release that still has to be fetched — exactly the
/// stall this ranking exists to avoid. When in doubt, report not-cached.
///
/// Returns `None` when neither marker is present (the keyless public endpoint,
/// or Torrentio's synthetic "RD error" stream). That means "unknown", which the
/// caller must not conflate with "uncached".
fn parse_debrid_cached(name: &str) -> Option<bool> {
    let start = name.find('[')?;
    let end = name[start..].find(']')? + start;
    let tag = &name[start + 1..end];

    // "WEB-DL"/"DL" can appear in titles; we only ever read the bracket tag, so
    // that ambiguity cannot reach here.
    if tag.to_ascii_lowercase().contains("download") {
        return Some(false);
    }
    if tag.trim_end().ends_with('+') {
        return Some(true);
    }
    None
}

/// Strip common video file extensions from a filename.
fn strip_file_extension(filename: &str) -> String {
    let extensions = [
        ".mkv", ".mp4", ".avi", ".wmv", ".m4v", ".ts", ".m2ts", ".flv", ".webm", ".mov",
    ];
    let lower = filename.to_lowercase();
    for ext in &extensions {
        if lower.ends_with(ext) {
            return filename[..filename.len() - ext.len()].to_string();
        }
    }
    filename.to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_debrid_cache_markers() {
        use super::parse_debrid_cached;

        // Exact live formats (verified against torrentio.strem.fun).
        assert_eq!(parse_debrid_cached("[RD+] Torrentio\n4k DV"), Some(true));
        assert_eq!(
            parse_debrid_cached("[RD download] Torrentio\n4k HDR"),
            Some(false)
        );

        // Torrent is cached but this file still needs fetching. Ends with '+'
        // AND contains "download" — must resolve to NOT cached, or we would
        // rank a release that still has to download as instantly playable.
        assert_eq!(
            parse_debrid_cached("[RD+ Download] Torrentio\n1080p"),
            Some(false)
        );

        // Other providers use the same convention.
        assert_eq!(parse_debrid_cached("[AD+] Torrentio"), Some(true));
        assert_eq!(parse_debrid_cached("[TB download] Torrentio"), Some(false));

        // Keyless public endpoint and Torrentio's synthetic error stream carry
        // no marker — "unknown", not "uncached".
        assert_eq!(parse_debrid_cached("Torrentio\n4k DV"), None);
        assert_eq!(parse_debrid_cached("Torrentio\nRD error"), None);
        assert_eq!(parse_debrid_cached(""), None);
    }

    #[test]
    fn parses_debrid_resolve_url() {
        use super::parse_resolve_url;

        // Debrid-mode responses omit infoHash/fileIdx; both survive only here:
        // /resolve/{moch}/{apiKey}/{infoHash}/{cachedEntryInfo}/{fileIdx}/{name}
        let url = "https://torrentio.strem.fun/resolve/realdebrid/TESTKEYTESTKEYTESTKEYTESTKEYTESTKEYTESTKEYTESTKEY2345/\
45fa4233ef87c58f5f8b4817e4d50c9f5363caef/null/0/Some.Movie.2160p.mkv";
        let got = parse_resolve_url(url).expect("should parse");
        assert_eq!(got.info_hash, "45fa4233ef87c58f5f8b4817e4d50c9f5363caef");
        assert_eq!(got.file_idx, Some(0));

        // A 40-char all-hex API key must not be mistaken for the hash.
        let hex_key = "https://torrentio.strem.fun/resolve/realdebrid/\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/\
45fa4233ef87c58f5f8b4817e4d50c9f5363caef/null/3/x.mkv";
        let got = parse_resolve_url(hex_key).expect("should parse");
        assert_eq!(got.info_hash, "45fa4233ef87c58f5f8b4817e4d50c9f5363caef");
        assert_eq!(got.file_idx, Some(3));

        // Not a resolve URL / truncated.
        assert!(parse_resolve_url("https://x/resolve/rd/K").is_none());
        assert!(parse_resolve_url("").is_none());
    }

    use super::*;

    #[test]
    fn test_strip_file_extension() {
        assert_eq!(
            strip_file_extension("Black.Rabbit.S01E01.1080p.WEB-DL.mkv"),
            "Black.Rabbit.S01E01.1080p.WEB-DL"
        );
        assert_eq!(
            strip_file_extension("Movie.2025.2160p.WEB-DL.mp4"),
            "Movie.2025.2160p.WEB-DL"
        );
        // Unknown extension kept as-is
        assert_eq!(
            strip_file_extension("Black.Rabbit.S01E01.1080p.WEB-DL"),
            "Black.Rabbit.S01E01.1080p.WEB-DL"
        );
    }

    // -- resolve_title tests --

    #[test]
    fn test_filename_with_full_info_unchanged() {
        // Filename has show + episode + quality — use as-is
        let result = resolve_title(
            Some("Law & Order (1990) - S08E12 - Expert (1080p AMZN WEB-DL x265 ImE)"),
            Some(
                "Law & Order (1990) Season 8 S08 (1080p AMZN WEB-DL x265 HEVC 10bit EAC3 2.0 ImE) [QxR]",
            ),
            "tv",
            Some(8),
            Some(12),
        );
        assert_eq!(
            result.unwrap(),
            "Law & Order (1990) - S08E12 - Expert (1080p AMZN WEB-DL x265 ImE)"
        );
    }

    #[test]
    fn test_bare_filename_synthesized_from_pack() {
        // "812 - Expert" has no show name, no SxxExx — synthesize from pack
        let result = resolve_title(
            Some("812 - Expert"),
            Some("Law and Order - Season 8 Complete - HDTV - GCJM"),
            "tv",
            Some(8),
            Some(12),
        );
        let title = result.unwrap();
        // Should contain show name, episode marker, and quality from pack
        assert!(
            title.contains("Law and Order"),
            "should have show name: {title}"
        );
        assert!(title.contains("S08E12"), "should have episode: {title}");
        assert!(
            title.contains("hdtv"),
            "should have quality from pack: {title}"
        );
    }

    #[test]
    fn test_filename_with_episode_but_no_quality_enriched() {
        // "Law & Order - S08E12 - Expert" has show + episode but no quality
        let result = resolve_title(
            Some("Law & Order - S08E12 - Expert"),
            Some("Law & Order 1990 Seasons 1 to 23 Complete Mixed x264 [i_c]"),
            "tv",
            Some(8),
            Some(12),
        );
        let title = result.unwrap();
        assert!(
            title.starts_with("Law & Order - S08E12 - Expert"),
            "should keep original filename: {title}"
        );
        assert!(
            title.contains("avc"),
            "should have codec from pack: {title}"
        );
    }

    #[test]
    fn test_no_filename_uses_pack_name() {
        let result = resolve_title(
            None,
            Some("Law.and.Order.S08E12.720p.HDTV.x264-GROUP"),
            "tv",
            Some(8),
            Some(12),
        );
        assert_eq!(result.unwrap(), "Law.and.Order.S08E12.720p.HDTV.x264-GROUP");
    }

    #[test]
    fn test_movie_uses_filename_directly() {
        let result = resolve_title(
            Some("The.Movie.2024.1080p.WEB-DL"),
            Some("Some Pack Name"),
            "movies",
            None,
            None,
        );
        assert_eq!(result.unwrap(), "The.Movie.2024.1080p.WEB-DL");
    }

    #[test]
    fn test_filename_with_alt_format_and_quality() {
        // "Law & Order - 8x12 - Expert [PDTV-drngr]" — has episode (alt format) + quality
        let result = resolve_title(
            Some("Law & Order - 8x12 - Expert [PDTV-drngr]"),
            Some("Law & Order COMPLETE All 20 Seasons"),
            "tv",
            Some(8),
            Some(12),
        );
        assert_eq!(
            result.unwrap(),
            "Law & Order - 8x12 - Expert [PDTV-drngr]",
            "should keep filename as-is when it has episode + quality"
        );
    }

    #[test]
    fn test_synthesize_from_pack_with_quality() {
        let title = synthesize_from_pack("Law and Order - Season 8 Complete - HDTV - GCJM", 8, 12);
        assert!(title.contains("Law and Order"), "show name: {title}");
        assert!(title.contains("S08E12"), "episode: {title}");
        assert!(title.contains("hdtv"), "quality: {title}");
        // GCJM is after " - " not "-", so release group regex won't catch it.
        // The quality source (HDTV) is what matters for scoring.
    }

    #[test]
    fn test_synthesize_from_pack_with_dotted_group() {
        // Standard scene naming — group IS detected
        let title = synthesize_from_pack("Law.and.Order.S08.720p.HDTV.x264-LOL", 8, 12);
        assert!(title.contains("Law and Order"), "show name: {title}");
        assert!(title.contains("S08E12"), "episode: {title}");
        assert!(title.contains("720p"), "resolution: {title}");
        assert!(title.contains("hdtv"), "source: {title}");
        assert!(title.contains("LOL"), "group: {title}");
    }

    #[test]
    fn test_synthesize_from_pack_minimal() {
        // Pack with no quality tokens at all
        let title = synthesize_from_pack("Law & Order COMPLETE All 20 Seasons", 8, 12);
        assert!(title.contains("S08E12"), "episode: {title}");
        // clean_title strips "COMPLETE", "20", "Seasons" — show name is approximate
        // but that's fine, structural matching uses IMDB not title
    }

    // -- parse_title_metadata tests --

    #[test]
    fn test_parse_title_metadata_full() {
        let title = "Law & Order (1990) Season 8\nS08E12.mkv\n👤 3 💾 1.28 GB ⚙️ 1337x";
        let (seeders, size) = parse_title_metadata(title);
        assert_eq!(seeders, Some(3));
        assert_eq!(size, Some((1.28 * 1_073_741_824.0) as i64));
    }

    #[test]
    fn test_parse_title_metadata_mb() {
        let title = "Some.Movie\nfile.mkv\n👤 15 💾 314.18 MB ⚙️ rarbg";
        let (seeders, size) = parse_title_metadata(title);
        assert_eq!(seeders, Some(15));
        assert_eq!(size, Some((314.18 * 1_048_576.0) as i64));
    }

    #[test]
    fn test_parse_title_metadata_tb() {
        let title = "Pack\nfile.mkv\n👤 1 💾 2.5 TB";
        let (seeders, size) = parse_title_metadata(title);
        assert_eq!(seeders, Some(1));
        assert_eq!(size, Some((2.5 * 1_099_511_627_776.0) as i64));
    }

    #[test]
    fn test_parse_title_metadata_seeders_only() {
        let title = "Some title\n👤 42";
        let (seeders, size) = parse_title_metadata(title);
        assert_eq!(seeders, Some(42));
        assert_eq!(size, None);
    }

    #[test]
    fn test_parse_title_metadata_size_only() {
        let title = "Some title\n💾 4.7 GB";
        let (seeders, size) = parse_title_metadata(title);
        assert_eq!(seeders, None);
        assert_eq!(size, Some((4.7 * 1_073_741_824.0) as i64));
    }

    #[test]
    fn test_parse_title_metadata_no_emoji() {
        let title = "Just a plain title with no metadata";
        let (seeders, size) = parse_title_metadata(title);
        assert_eq!(seeders, None);
        assert_eq!(size, None);
    }

    #[test]
    fn test_parse_title_metadata_zero_seeders() {
        let title = "👤 0 💾 1.5 GB";
        let (seeders, size) = parse_title_metadata(title);
        assert_eq!(seeders, Some(0));
        assert_eq!(size, Some((1.5 * 1_073_741_824.0) as i64));
    }
}
