//! Media file interpretation and release name parsing.
//!
//! This module provides comprehensive parsing of release names, folder structures,
//! and file metadata for all media types supported by Kobload.
//!
//! # Overview
//!
//! The parsing module enables callers to understand:
//! - TV episodes (S01E05, daily shows, anime, miniseries, specials)
//! - Movies (year, edition, multi-part, 3D)
//! - Books (author, title, series)
//! - Audiobooks (narrator, unabridged status, parts)
//! - Music (artist, album, year, source)
//! - Games (platform, version, crack status)
//! - Software (version, platform, portable)
//! - XXX (studio, performers)
//!
//! # Usage
//!
//! The primary entry point is [`parse_release`], which takes a release name and
//! media kind, returning structured metadata:
//!
//! ```rust,ignore
//! use kobload_api::parsing::{parse_release, ParsedRelease};
//!
//! let parsed = parse_release("Show.S01E05.Title.1080p.WEB-DL.x265", "tv");
//! if let ParsedMedia::Tv(tv) = &parsed.media {
//!     assert_eq!(tv.season, Some(1));
//!     assert_eq!(tv.episodes, vec![5]);
//! }
//! ```
//!
//! For folder structure parsing, use [`parse_path`]:
//!
//! ```rust,ignore
//! use kobload_api::parsing::parse_path;
//!
//! let parsed = parse_path("Show Name/Season 01/S01E05.mkv", "tv");
//! assert_eq!(parsed.resolved.media.as_tv().unwrap().season, Some(1));
//! ```

mod audiobook;
mod book;
pub(crate) mod common;
pub(crate) mod file;
mod games;
mod movie;
mod music;
mod path;
mod software;
mod sport;
mod tv;
mod types;
mod xxx;

pub use file::{
    AudioMetadata, FileClassification, FileMetadata, FileRole, MetadataExtractionStatus,
    SubtitleMetadata, VideoMetadata, classify_file, classify_filename, classify_files,
    extract_file_metadata, is_archive_nested,
};
pub use path::{FolderContext, PathParsed, parse_path};
pub use sport::{canonical_sport_text, country_for_code, country_names};
pub use types::*;

use crate::quality::parse_quality;

/// Parse a release name into structured metadata.
///
/// This is the primary entry point for release name interpretation. The caller
/// provides the `media_kind` to guide parsing - we don't try to auto-detect
/// whether something is a TV show vs movie.
///
/// # Arguments
///
/// * `name` - The release name to parse (e.g., "Show.S01E05.1080p.WEB-DL")
/// * `media_kind` - The media type: "tv", "movies", "ebooks", "audiobooks", "music", "games", "software", "xxx", "misc"
///
/// # Returns
///
/// A [`ParsedRelease`] containing:
/// - Cleaned title
/// - Media-specific metadata (episodes, year, author, etc.)
/// - Quality information (resolution, source, codec, etc.)
/// - Release flags (PROPER, REPACK, etc.)
/// - Release group name
/// - Confidence score
///
/// # Example
///
/// ```rust,ignore
/// let parsed = parse_release("Game.of.Thrones.S01E01.1080p.BluRay.x264-GROUP", "tv");
/// assert_eq!(parsed.title, "Game of Thrones");
/// if let ParsedMedia::Tv(tv) = parsed.media {
///     assert_eq!(tv.season, Some(1));
///     assert_eq!(tv.episodes, vec![1]);
/// }
/// ```
pub fn parse_release(name: &str, media_kind: &str) -> ParsedRelease {
    // Strip torrent site watermarks before parsing (e.g. "www.UIndex.org - ", "[EZTVx.to]")
    let name = common::strip_site_watermarks(&common::repair_apostrophes(name));
    let name = name.as_ref();

    // Parse quality information (resolution, source, codec, flags, etc.)
    let quality = parse_quality(name);

    // Parse media-specific information based on kind
    let (title, media, confidence) = match media_kind {
        "tv" => tv::parse_tv(name),
        "movies" => movie::parse_movie(name),
        "ebooks" => book::parse_book(name),
        "audiobooks" => audiobook::parse_audiobook(name),
        "music" => music::parse_music(name),
        "games" => games::parse_game(name),
        "software" => software::parse_software(name),
        "xxx" => xxx::parse_xxx(name),
        "sports" => sport::parse_sport(name),
        _ => common::parse_unknown(name),
    };

    // Build release flags from quality parsing
    let flags = ReleaseFlags {
        is_proper: quality.is_proper,
        is_repack: quality.is_repack,
        is_real: quality.is_real,
        is_remux: quality.is_remux,
        is_internal: quality.is_internal,
        is_retail: quality.is_retail,
        is_limited: quality.is_limited,
        is_nfo_fix: quality.is_nfo_fix,
        is_sample_fix: quality.is_sample_fix,
        is_sync_fix: quality.is_sync_fix,
        is_proof: quality.is_proof,
        is_dirfix: quality.is_dirfix,
        is_nuked: quality.is_nuked,
        is_subbed: quality.is_subbed,
        is_dubbed: quality.is_dubbed,
        is_hardcoded_subs: quality.is_hardcoded_subs,
        is_hdr: quality.is_hdr,
        is_hdr10: quality.is_hdr10,
        is_hdr10plus: quality.is_hdr10plus,
        is_dolby_vision: quality.is_dolby_vision,
        is_atmos: quality.is_atmos,
        is_dtsx: quality.is_dtsx,
    };

    let release_group = quality.release_group.clone();

    ParsedRelease {
        title,
        media,
        quality,
        flags,
        release_group,
        confidence,
    }
}

/// Heuristic to guess media kind from a release title.
///
/// Returns a media kind string suitable for [`parse_release`].
pub fn guess_media_kind(title: &str) -> &'static str {
    let lower = title.to_lowercase();
    if lower.contains(".s0")
        || lower.contains(".s1")
        || lower.contains(".s2")
        || lower.contains(".s3")
        || lower.contains("season")
        || lower.contains("episode")
        || lower.contains(".e0")
    {
        "tv"
    } else {
        "movies"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_release_tv() {
        let parsed = parse_release(
            "Game.of.Thrones.S01E05.The.Wolf.and.the.Lion.1080p.BluRay.x264-DEMAND",
            "tv",
        );
        assert_eq!(parsed.title, "Game of Thrones");
        assert_eq!(parsed.release_group.as_deref(), Some("DEMAND"));

        if let ParsedMedia::Tv(tv) = &parsed.media {
            assert_eq!(tv.season, Some(1));
            assert_eq!(tv.episodes, vec![5]);
            assert!(!tv.is_pack);
        } else {
            panic!("Expected TV parsed media");
        }
    }

    #[test]
    fn test_parse_release_movie() {
        let parsed = parse_release("Inception.2010.1080p.BluRay.x264-GROUP", "movies");
        assert_eq!(parsed.title, "Inception");

        if let ParsedMedia::Movie(movie) = &parsed.media {
            assert_eq!(movie.year, Some(2010));
        } else {
            panic!("Expected Movie parsed media");
        }
    }

    #[test]
    fn test_parse_release_with_quality() {
        let parsed = parse_release("Movie.2024.2160p.DV.WEB-DL.x265.Atmos-GROUP", "movies");
        assert!(parsed.flags.is_dolby_vision);
        assert!(parsed.flags.is_atmos);
        assert_eq!(parsed.quality.resolution.as_deref(), Some("2160p"));
        assert_eq!(parsed.quality.source.as_deref(), Some("web-dl"));
    }

    // ── Parser confidence tests for structural_reject adversarial inputs ──
    //
    // These verify the parser extracts the correct fields from release names
    // that structural_reject depends on. If any of these fail, structural_reject
    // silently fails to catch false positives.

    // ── Movies: year extraction ───────────────────────────────────────

    #[test]
    fn test_parser_movie_toy_story_1995() {
        let parsed = parse_release(
            "Toy.Story.1995.REPACK.720p.BluRay.DD+5.1.x264-playHD",
            "movies",
        );
        let movie = parsed.media.as_movie().expect("Should parse as movie");
        assert_eq!(movie.year, Some(1995), "Parser must extract year 1995");
    }

    #[test]
    fn test_parser_movie_dune_1984() {
        let parsed = parse_release("Dune.1984.1080p.BluRay.x264-GROUP", "movies");
        let movie = parsed.media.as_movie().expect("Should parse as movie");
        assert_eq!(movie.year, Some(1984), "Parser must extract year 1984");
    }

    #[test]
    fn test_parser_movie_2001_space_odyssey() {
        // "2001" is in the title, but "1968" is the release year
        let parsed = parse_release(
            "2001.A.Space.Odyssey.1968.1080p.BluRay.x264-GROUP",
            "movies",
        );
        let movie = parsed.media.as_movie().expect("Should parse as movie");
        assert_eq!(
            movie.year,
            Some(1968),
            "Parser must extract 1968 as year, not 2001 from the title"
        );
    }

    #[test]
    fn test_parser_movie_aka_blade_1995() {
        let parsed = parse_release(
            "Dao.AKA.The.Blade.REPACK.1995.1080p.NF.WEB-DL.AAC2.0.x264-Gohan",
            "movies",
        );
        let movie = parsed.media.as_movie().expect("Should parse as movie");
        assert_eq!(
            movie.year,
            Some(1995),
            "Parser must extract year from AKA title"
        );
    }

    #[test]
    fn test_parser_movie_jurassic_park_remaster() {
        let parsed = parse_release(
            "Jurassic.Park.1993.30th.Anniversary.4K.Remaster.2160p.BluRay-GROUP",
            "movies",
        );
        let movie = parsed.media.as_movie().expect("Should parse as movie");
        assert_eq!(
            movie.year,
            Some(1993),
            "Parser must extract original year, not confuse with '30th'"
        );
    }

    #[test]
    fn test_parser_movie_mario_bros_2023() {
        let parsed = parse_release(
            "The.Super.Mario.Bros.Movie.2023.2160p.REMUX.HEVC.HDR10.TrueHD.7.1-samhydetorrentgalaxy",
            "movies",
        );
        let movie = parsed.media.as_movie().expect("Should parse as movie");
        assert_eq!(
            movie.year,
            Some(2023),
            "Parser must extract 2023, not confuse with HDR10"
        );
    }

    #[test]
    fn test_parser_movie_no_year() {
        let parsed = parse_release("Movie.Title.1080p.BluRay.x264-GROUP", "movies");
        let movie = parsed.media.as_movie().expect("Should parse as movie");
        assert_eq!(
            movie.year, None,
            "No year in release — parser should return None"
        );
    }

    // ── TV: season/episode extraction ─────────────────────────────────

    #[test]
    fn test_parser_tv_standard_episode() {
        let parsed = parse_release("Breaking.Bad.S01E05.1080p.BluRay.x264-GROUP", "tv");
        let tv = parsed.media.as_tv().expect("Should parse as TV");
        assert_eq!(tv.season, Some(1));
        assert_eq!(tv.episodes, vec![5]);
        assert!(!tv.is_pack);
        assert!(!tv.is_daily);
    }

    #[test]
    fn test_parser_tv_wrong_episode_number() {
        // Verify parser correctly extracts E10, not E05
        let parsed = parse_release("Show.S01E10.1080p.WEB-DL-GROUP", "tv");
        let tv = parsed.media.as_tv().expect("Should parse as TV");
        assert_eq!(tv.season, Some(1));
        assert_eq!(tv.episodes, vec![10]);
    }

    #[test]
    fn test_parser_tv_multi_episode() {
        let parsed = parse_release("Show.S01E05E06E07.1080p.WEB-DL-GROUP", "tv");
        let tv = parsed.media.as_tv().expect("Should parse as TV");
        assert_eq!(tv.season, Some(1));
        assert!(
            tv.episodes.contains(&5) && tv.episodes.contains(&6) && tv.episodes.contains(&7),
            "Multi-ep should contain all episodes: got {:?}",
            tv.episodes
        );
    }

    #[test]
    fn test_parser_tv_season_pack() {
        let parsed = parse_release("Show.S01.Complete.1080p.BluRay-GROUP", "tv");
        let tv = parsed.media.as_tv().expect("Should parse as TV");
        assert_eq!(tv.season, Some(1));
        assert!(tv.is_pack, "Should detect as season pack");
    }

    #[test]
    fn test_parser_tv_daily_show() {
        let parsed = parse_release("The.Daily.Show.2024.01.15.720p.HDTV-GROUP", "tv");
        let tv = parsed.media.as_tv().expect("Should parse as TV");
        assert!(tv.is_daily, "Should detect as daily show");
        // Daily shows should NOT have standard season/episode numbers
        // that would trigger false rejection
    }

    #[test]
    fn test_parser_tv_anime_absolute() {
        let parsed = parse_release("[SubGroup] Naruto - 100 [720p].mkv", "tv");
        let tv = parsed.media.as_tv().expect("Should parse as TV");
        // Anime absolute numbering — should have absolute_episode or episodes
        // but no standard season, so structural_reject won't fire
        assert!(
            tv.absolute_episode.is_some() || tv.season.is_none(),
            "Anime should use absolute numbering or have no season"
        );
    }

    // ── Sport: season/round extraction ────────────────────────────────

    #[test]
    fn test_parser_sport_motogp() {
        let parsed = parse_release("MotoGP.2026.R02.Brazil.Race.1080p.WEB-DL", "sports");
        let sport = parsed.media.as_sport().expect("Should parse as sport");
        assert_eq!(sport.season, Some(2026), "Season year must be 2026");
        assert_eq!(sport.round, Some(2), "Round must be 2");
        assert_eq!(
            sport.session_type.as_deref(),
            Some("Race"),
            "Session type must be Race"
        );
    }

    #[test]
    fn test_parser_sport_wrong_round() {
        let parsed = parse_release("MotoGP.2026.R03.Thailand.Race.1080p.WEB-DL", "sports");
        let sport = parsed.media.as_sport().expect("Should parse as sport");
        assert_eq!(sport.season, Some(2026));
        assert_eq!(sport.round, Some(3), "Round must be 3, not 2");
    }

    #[test]
    fn test_parser_sport_f1() {
        let parsed = parse_release(
            "Formula.1.2026.R05.Monaco.Qualifying.1080p.WEB-DL-GROUP",
            "sports",
        );
        let sport = parsed.media.as_sport().expect("Should parse as sport");
        assert_eq!(sport.season, Some(2026));
        assert_eq!(sport.round, Some(5));
    }

    // ── Music: year extraction ────────────────────────────────────────

    #[test]
    fn test_parser_music_radiohead_1997() {
        let parsed = parse_release("Radiohead-OK_Computer-1997-FLAC-GROUP", "music");
        let music = parsed.media.as_music().expect("Should parse as music");
        assert_eq!(music.year, Some(1997), "Year must be 1997");
    }

    #[test]
    fn test_parser_music_radiohead_2003() {
        let parsed = parse_release("Radiohead-OK_Computer-2003-FLAC-GROUP", "music");
        let music = parsed.media.as_music().expect("Should parse as music");
        assert_eq!(music.year, Some(2003), "Year must be 2003");
    }

    // ── Site watermark stripping ───────────────────────────────────────

    #[test]
    fn test_site_prefix_stripping() {
        // www.SiteName.org - prefix
        let parsed = parse_release(
            "www.UIndex.org    -    Black.Rabbit.S01E01.1080p.WEB.h264-ETHEL",
            "tv",
        );
        assert_eq!(parsed.title, "Black Rabbit");
        assert_eq!(parsed.release_group.as_deref(), Some("ETHEL"));
        let tv = parsed.media.as_tv().unwrap();
        assert_eq!(tv.season, Some(1));
        assert_eq!(tv.episodes, vec![1]);

        // www.SiteName.com - prefix (different domain)
        let parsed = parse_release(
            "www.FiberMovies.com - Black.Rabbit.2025.S01.1080p.Hindi.Dual.WEB.HDRip",
            "tv",
        );
        assert!(
            parsed.title.starts_with("Black Rabbit"),
            "Site prefix should be stripped, got: {}",
            parsed.title,
        );

        // Chinese bracket watermark 【...www.BPHDTV.com】
        let parsed = parse_release(
            "【高清剧集网发布 www.BPHDTV.com】黑兔[全8集].Black.Rabbit.S01.2025.1080p.NF.WEB-DL-Color",
            "tv",
        );
        assert!(
            !parsed.title.contains("BPHDTV"),
            "Chinese site watermark should be stripped, got: {}",
            parsed.title,
        );
    }

    #[test]
    fn test_site_suffix_stripping() {
        // [EZTVx.to] suffix should be stripped, revealing the real group
        let parsed = parse_release("Black.Rabbit.S01E01.1080p.WEB.h264-ETHEL[EZTVx.to]", "tv");
        assert_eq!(
            parsed.release_group.as_deref(),
            Some("ETHEL"),
            "Site suffix should be stripped to reveal real group",
        );

        // .mkv extension + site suffix
        let parsed = parse_release(
            "Black.Rabbit.S01E01.1080p.WEB.h264-ETHEL[EZTVx.to].mkv",
            "tv",
        );
        assert_eq!(parsed.release_group.as_deref(), Some("ETHEL"));
    }

    #[test]
    fn test_site_stripping_does_not_affect_normal_titles() {
        // Normal title without watermarks should be unaffected
        let parsed = parse_release(
            "Black.Rabbit.S01E01.1080p.NF.WEB-DL.DDP5.1.Atmos.H.264-FLUX",
            "tv",
        );
        assert_eq!(parsed.title, "Black Rabbit");
        assert_eq!(parsed.release_group.as_deref(), Some("FLUX"));

        // Bracket group names that don't look like domains should survive
        let parsed = parse_release(
            "Black Rabbit 2025 Season 1 Complete 1080p WEB x264 [i_c]",
            "tv",
        );
        // [i_c] doesn't look like a domain — no dot+TLD — should not be stripped
        assert!(
            !parsed.title.is_empty(),
            "Normal bracket group should not cause empty title",
        );
    }
}
