//! Episode key extraction and matching for TvSeason watch patterns.
//!
//! When a TvSeason pattern matches a release, we extract an "episode key" that
//! identifies which episode(s) the release satisfies. This enables per-episode
//! download state tracking within a single season pattern.
//!
//! Key formats:
//! - `"S01E05"` — single episode
//! - `"S01E05-E07"` — multi-episode range
//! - `"SEASON_PACK"` — full season pack

use crate::parsing::{ParsedMedia, parse_release};

/// Sentinel key for season pack releases.
pub const SEASON_PACK_KEY: &str = "SEASON_PACK";

/// Extract an episode key from a release title, if the pattern is a TvSeason.
///
/// Returns `None` if:
/// - `media_want` is not a `TvSeason` variant
/// - The release title can't be parsed into season/episode info
pub fn extract_episode_key(release_title: &str, media_want: &serde_json::Value) -> Option<String> {
    // Only applies to TvSeason patterns
    if !is_tv_season(media_want) {
        return None;
    }

    let parsed = parse_release(release_title, "tv");
    let tv = match parsed.media {
        ParsedMedia::Tv(tv) => tv,
        _ => return None,
    };

    if tv.is_pack {
        return Some(SEASON_PACK_KEY.to_string());
    }

    // Fallback for anime absolute numbering (no season, has absolute_episode)
    if tv.season.is_none()
        && let Some(abs) = tv.absolute_episode
    {
        return Some(format!("ABS{:04}", abs));
    }

    let season = tv.season?;

    if tv.episodes.is_empty() {
        return None;
    }

    if tv.episodes.len() == 1 {
        return Some(format!("S{:02}E{:02}", season, tv.episodes[0]));
    }

    // Multi-episode: S01E05-E07
    let min = tv.episodes.iter().min().copied()?;
    let max = tv.episodes.iter().max().copied()?;
    Some(format!("S{:02}E{:02}-E{:02}", season, min, max))
}

/// Check if a `media_want` JSON value represents a TvSeason.
///
/// `MediaWant` uses `#[serde(tag = "kind", rename_all = "snake_case")]` (internally tagged),
/// so TvSeason serializes as `{"kind": "tv_season", ...}`.
pub fn is_tv_season(media_want: &serde_json::Value) -> bool {
    media_want.get("kind").and_then(|v| v.as_str()) == Some("tv_season")
}

/// Get the `episodes_needed` from a TvSeason media_want, if present.
pub fn get_episodes_needed(media_want: &serde_json::Value) -> Option<Vec<nats_jobs::EpisodeRef>> {
    if !is_tv_season(media_want) {
        return None;
    }
    let episodes = media_want.get("episodes_needed")?;
    serde_json::from_value(episodes.clone()).ok()
}

/// Check if a release's episode key covers an episode that is still needed.
///
/// - `SEASON_PACK` is always needed (covers everything).
/// - Single/range keys check if any covered episode number appears in `episodes_needed`.
pub fn is_episode_needed(episode_key: &str, episodes_needed: &[nats_jobs::EpisodeRef]) -> bool {
    if episode_key == SEASON_PACK_KEY {
        return true;
    }
    let covered = episodes_from_key(episode_key);
    covered.iter().any(|ep_str| {
        episodes_needed
            .iter()
            .any(|needed| needed.episode == *ep_str)
    })
}

/// Check if an episode_key covers a specific episode identifier.
///
/// `episode_identifier` is the string from `EpisodeRef.episode` (e.g., `"5"`, `"12"`).
/// `SEASON_PACK` covers everything.
pub fn key_covers_episode(key: &str, episode_identifier: &str) -> bool {
    if key == SEASON_PACK_KEY {
        return true;
    }

    let ep_num: u32 = match episode_identifier.parse() {
        Ok(n) => n,
        Err(_) => return false,
    };

    for covered in episodes_from_key(key) {
        if let Ok(n) = covered.parse::<u32>()
            && n == ep_num
        {
            return true;
        }
    }
    false
}

/// Build synthetic episode keys for episodes already marked `in_library: true`
/// in the media_want. These can be merged with Kobload's imported-episode keys
/// so that coverage checks (e.g. `all_episodes_covered`) treat pre-existing
/// library files as covered without requiring a Kobload download record.
pub fn in_library_episode_keys(episodes_needed: &[nats_jobs::EpisodeRef]) -> Vec<String> {
    episodes_needed
        .iter()
        .filter(|ep| ep.in_library == Some(true))
        .filter_map(|ep| {
            let season: u32 = ep.season.parse().ok()?;
            let episode: u32 = ep.episode.parse().ok()?;
            Some(format!("S{season:02}E{episode:02}"))
        })
        .collect()
}

/// Check library coverage for all episodes covered by an episode key.
///
/// Returns `Some((all_in_library, min_video_height))` when AT LEAST ONE covered
/// episode is in the library, so quality gating can run. `all_in_library` is
/// `true` only when every covered episode is already held — for a season pack
/// that fills a single missing finale it is `false`, but the returned height
/// still reflects the held episodes so a downgrade pack can be rejected.
/// Returns `None` only when NONE of the covered episodes are in the library
/// (a genuinely new download — nothing to compare against).
///
/// For season packs: checks every episode in `episodes_needed`.
/// For multi-episode keys (e.g. S01E05E06): checks all covered episodes.
/// For single-episode keys: checks that one episode.
///
/// The returned `min_video_height` is the WORST (lowest) height across the
/// IN-LIBRARY covered episodes, so downgrade decisions are conservative. If any
/// in-library episode is unprobed (`library_video_height: None`), the overall
/// height is `None`.
pub fn check_library_coverage(
    episode_key: &str,
    episodes_needed: &[nats_jobs::EpisodeRef],
) -> Option<(bool, Option<i32>)> {
    let covered_episodes: Vec<&nats_jobs::EpisodeRef> = if episode_key == SEASON_PACK_KEY {
        // Season pack covers everything
        episodes_needed.iter().collect()
    } else {
        episodes_needed
            .iter()
            .filter(|ep| key_covers_episode(episode_key, &ep.episode))
            .collect()
    };

    if covered_episodes.is_empty() {
        return None;
    }

    let in_library: Vec<&nats_jobs::EpisodeRef> = covered_episodes
        .iter()
        .copied()
        .filter(|ep| ep.in_library == Some(true))
        .collect();

    // None of the covered episodes are in library — genuinely new, nothing to gate on
    if in_library.is_empty() {
        return None;
    }

    let all_in_library = in_library.len() == covered_episodes.len();

    // Worst (lowest) quality across the IN-LIBRARY episodes. A pack that would
    // replace these with something lower is a downgrade regardless of whether it
    // also fills genuinely-missing episodes.
    let min_height = in_library.iter().try_fold(i32::MAX, |acc, ep| {
        ep.library_video_height.map(|h| acc.min(h))
    });

    // If any in-library episode is unprobed (None height), overall height is None
    Some((all_in_library, min_height.filter(|&h| h != i32::MAX)))
}

/// Expand an episode_key to the list of episode number strings it covers.
///
/// - `"S01E05"` → `["5"]`
/// - `"S01E05-E07"` → `["5", "6", "7"]`
/// - `"SEASON_PACK"` → empty vec (means "all" — callers handle this specially)
pub fn episodes_from_key(key: &str) -> Vec<String> {
    if key == SEASON_PACK_KEY {
        return Vec::new();
    }

    // ABS{nnnn} — anime absolute numbering
    let upper = key.to_uppercase();
    if let Some(abs_str) = upper.strip_prefix("ABS") {
        if let Ok(n) = abs_str.parse::<u32>() {
            return vec![n.to_string()];
        }
        return Vec::new();
    }

    // Parse S{ss}E{ee} or S{ss}E{ee}-E{ff}
    let after_s = match upper.strip_prefix('S') {
        Some(rest) => rest,
        None => return Vec::new(),
    };

    // Skip season digits to find 'E'
    let e_pos = match after_s.find('E') {
        Some(p) => p,
        None => return Vec::new(),
    };

    let ep_part = &after_s[e_pos + 1..];

    if let Some(dash_pos) = ep_part.find("-E") {
        // Multi-episode range: "05-E07"
        let start_str = &ep_part[..dash_pos];
        let end_str = &ep_part[dash_pos + 2..];
        if let (Ok(start), Ok(end)) = (start_str.parse::<u32>(), end_str.parse::<u32>()) {
            return (start..=end).map(|n| n.to_string()).collect();
        }
    } else if let Ok(ep) = ep_part.parse::<u32>() {
        // Single episode: "05"
        return vec![ep.to_string()];
    }

    Vec::new()
}

/// Check if a set of imported episode keys fully covers all episodes_needed.
///
/// Accounts for SEASON_PACK (covers everything) and multi-episode keys.
pub fn all_episodes_covered(
    imported_keys: &[String],
    episodes_needed: &[nats_jobs::EpisodeRef],
) -> bool {
    if episodes_needed.is_empty() {
        // No specific episodes tracked — can't determine coverage
        return false;
    }

    // SEASON_PACK covers everything
    if imported_keys.iter().any(|k| k == SEASON_PACK_KEY) {
        return true;
    }

    episodes_needed.iter().all(|ep| {
        imported_keys
            .iter()
            .any(|key| key_covers_episode(key, &ep.episode))
    })
}

/// Returns `true` if the episode is missing from the library and should be
/// searched for in the per-episode fallback.  Episodes already in the library
/// are skipped — upgrades are handled via RSS, not backlog search.
pub fn episode_needs_search(ep: &nats_jobs::EpisodeRef) -> bool {
    ep.in_library != Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_single_episode() {
        let mw = serde_json::json!({"kind": "tv_season", "show_name": "Test", "season": "1"});
        assert_eq!(
            extract_episode_key("Test.S01E05.720p.WEB-DL", &mw),
            Some("S01E05".to_string())
        );
    }

    #[test]
    fn test_extract_multi_episode() {
        let mw = serde_json::json!({"kind": "tv_season", "show_name": "Test", "season": "1"});
        assert_eq!(
            extract_episode_key("Test.S01E05-E07.720p", &mw),
            Some("S01E05-E07".to_string())
        );
    }

    #[test]
    fn test_extract_season_pack() {
        let mw = serde_json::json!({"kind": "tv_season", "show_name": "Test", "season": "1"});
        assert_eq!(
            extract_episode_key("Test.S01.Complete.720p", &mw),
            Some("SEASON_PACK".to_string())
        );
    }

    #[test]
    fn test_extract_non_tv_season() {
        let mw = serde_json::json!({"kind": "tv_episode", "show_name": "Test"});
        assert_eq!(extract_episode_key("Test.S01E05.720p", &mw), None);
    }

    #[test]
    fn test_is_episode_needed() {
        let episodes_needed = vec![
            nats_jobs::EpisodeRef {
                season: "1".to_string(),
                episode: "2".to_string(),
                sort_order: Some(2),
                air_date: None,
                in_library: None,
                library_video_height: None,
                title: None,
            },
            nats_jobs::EpisodeRef {
                season: "1".to_string(),
                episode: "3".to_string(),
                sort_order: Some(3),
                air_date: None,
                in_library: None,
                library_video_height: None,
                title: None,
            },
        ];

        // Episode 1 is NOT needed (already owned)
        assert!(!is_episode_needed("S01E01", &episodes_needed));
        // Episode 2 IS needed
        assert!(is_episode_needed("S01E02", &episodes_needed));
        // Multi-episode range covering a needed episode
        assert!(is_episode_needed("S01E02-E04", &episodes_needed));
        // Multi-episode range NOT covering any needed episode
        assert!(!is_episode_needed("S01E04-E06", &episodes_needed));
        // Season pack is always needed
        assert!(is_episode_needed(SEASON_PACK_KEY, &episodes_needed));
    }

    #[test]
    fn test_get_episodes_needed() {
        let mw = serde_json::json!({
            "kind": "tv_season",
            "show_name": "Test",
            "season": "1",
            "episodes_needed": [
                {"season": "1", "episode": "2", "sort_order": 2},
                {"season": "1", "episode": "3", "sort_order": 3}
            ]
        });
        let eps = get_episodes_needed(&mw).unwrap();
        assert_eq!(eps.len(), 2);
        assert_eq!(eps[0].episode, "2");
        assert_eq!(eps[1].episode, "3");

        // Non-TvSeason returns None
        let mw_book = serde_json::json!({"kind": "book", "title": "Test"});
        assert!(get_episodes_needed(&mw_book).is_none());

        // TvSeason without episodes_needed returns None
        let mw_no_eps =
            serde_json::json!({"kind": "tv_season", "show_name": "Test", "season": "1"});
        assert!(get_episodes_needed(&mw_no_eps).is_none());
    }

    #[test]
    fn test_episodes_from_key_single() {
        assert_eq!(episodes_from_key("S01E05"), vec!["5"]);
    }

    #[test]
    fn test_episodes_from_key_range() {
        assert_eq!(episodes_from_key("S01E05-E07"), vec!["5", "6", "7"]);
    }

    #[test]
    fn test_episodes_from_key_season_pack() {
        assert!(episodes_from_key(SEASON_PACK_KEY).is_empty());
    }

    #[test]
    fn test_key_covers_episode() {
        assert!(key_covers_episode("S01E05", "5"));
        assert!(!key_covers_episode("S01E05", "6"));
        assert!(key_covers_episode("S01E05-E07", "5"));
        assert!(key_covers_episode("S01E05-E07", "6"));
        assert!(key_covers_episode("S01E05-E07", "7"));
        assert!(!key_covers_episode("S01E05-E07", "8"));
        assert!(key_covers_episode(SEASON_PACK_KEY, "1"));
        assert!(key_covers_episode(SEASON_PACK_KEY, "99"));
    }

    #[test]
    fn test_extract_anime_absolute_episode() {
        let mw = serde_json::json!({"kind": "tv_season", "show_name": "Naruto", "season": "1"});
        let key = extract_episode_key("[Group] Anime Title - 123 [1080p].mkv", &mw);
        assert_eq!(key, Some("ABS0123".to_string()));
    }

    #[test]
    fn test_extract_standard_episode_still_works() {
        let mw = serde_json::json!({"kind": "tv_season", "show_name": "Test", "season": "1"});
        assert_eq!(
            extract_episode_key("Test.S01E05.720p.WEB-DL", &mw),
            Some("S01E05".to_string())
        );
    }

    #[test]
    fn test_extract_season_pack_still_works() {
        let mw = serde_json::json!({"kind": "tv_season", "show_name": "Test", "season": "1"});
        assert_eq!(
            extract_episode_key("Test.S01.Complete.720p", &mw),
            Some("SEASON_PACK".to_string())
        );
    }

    #[test]
    fn test_episodes_from_key_abs() {
        assert_eq!(episodes_from_key("ABS0123"), vec!["123"]);
        assert_eq!(episodes_from_key("ABS0001"), vec!["1"]);
    }

    #[test]
    fn test_episode_needs_search() {
        let make_ep = |in_library: Option<bool>| nats_jobs::EpisodeRef {
            season: "1".to_string(),
            episode: "1".to_string(),
            sort_order: Some(1),
            air_date: None,
            in_library,
            library_video_height: None,
            title: None,
        };

        // Unknown library status — assume missing, must search
        assert!(episode_needs_search(&make_ep(None)));
        // Explicitly not in library — must search
        assert!(episode_needs_search(&make_ep(Some(false))));
        // In library — skip
        assert!(!episode_needs_search(&make_ep(Some(true))));
    }

    fn ep(episode: u32, in_library: Option<bool>, height: Option<i32>) -> nats_jobs::EpisodeRef {
        nats_jobs::EpisodeRef {
            season: "1".to_string(),
            episode: episode.to_string(),
            sort_order: Some(episode as i32),
            air_date: None,
            in_library,
            library_video_height: height,
            title: None,
        }
    }

    #[test]
    fn test_check_library_coverage_pack_all_in_library() {
        let eps = vec![ep(1, Some(true), Some(1080)), ep(2, Some(true), Some(1080))];
        assert_eq!(
            check_library_coverage(SEASON_PACK_KEY, &eps),
            Some((true, Some(1080)))
        );
    }

    #[test]
    fn test_check_library_coverage_pack_partial_keeps_held_height() {
        // 17 held at 1080, 1 missing finale: not all-in-library, but the held
        // height is still reported so a downgrade pack can be rejected.
        let mut eps: Vec<_> = (1..=17).map(|n| ep(n, Some(true), Some(1080))).collect();
        eps.push(ep(18, Some(false), None));
        assert_eq!(
            check_library_coverage(SEASON_PACK_KEY, &eps),
            Some((false, Some(1080)))
        );
    }

    #[test]
    fn test_check_library_coverage_pack_none_in_library() {
        let eps = vec![ep(1, Some(false), None), ep(2, None, None)];
        assert_eq!(check_library_coverage(SEASON_PACK_KEY, &eps), None);
    }

    #[test]
    fn test_check_library_coverage_partial_unprobed_height() {
        // One held episode is unprobed → overall height unknown.
        let eps = vec![
            ep(1, Some(true), None),
            ep(2, Some(true), Some(1080)),
            ep(3, Some(false), None),
        ];
        assert_eq!(
            check_library_coverage(SEASON_PACK_KEY, &eps),
            Some((false, None))
        );
    }

    #[test]
    fn test_check_library_coverage_single_episode_not_in_library() {
        let eps = vec![ep(5, Some(false), None)];
        assert_eq!(check_library_coverage("S01E05", &eps), None);
    }

    #[test]
    fn test_all_episodes_covered() {
        let eps = vec![
            nats_jobs::EpisodeRef {
                season: "1".to_string(),
                episode: "1".to_string(),
                sort_order: Some(1),
                air_date: None,
                in_library: None,
                library_video_height: None,
                title: None,
            },
            nats_jobs::EpisodeRef {
                season: "1".to_string(),
                episode: "2".to_string(),
                sort_order: Some(2),
                air_date: None,
                in_library: None,
                library_video_height: None,
                title: None,
            },
            nats_jobs::EpisodeRef {
                season: "1".to_string(),
                episode: "3".to_string(),
                sort_order: Some(3),
                air_date: None,
                in_library: None,
                library_video_height: None,
                title: None,
            },
        ];

        // Partial coverage
        assert!(!all_episodes_covered(&["S01E01".to_string()], &eps));

        // Full coverage
        assert!(all_episodes_covered(
            &[
                "S01E01".to_string(),
                "S01E02".to_string(),
                "S01E03".to_string()
            ],
            &eps
        ));

        // Season pack covers all
        assert!(all_episodes_covered(&[SEASON_PACK_KEY.to_string()], &eps));

        // Multi-ep range covers
        assert!(all_episodes_covered(&["S01E01-E03".to_string()], &eps));
    }
}
