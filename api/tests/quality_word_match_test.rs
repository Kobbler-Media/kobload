//! Token matching in `parse_quality`.
//!
//! Tokens used to be compiled into a regex per call without escaping, so the
//! `+` in `hdr10+`, `disney+` and `paramount+` acted as a quantifier and those
//! tokens could never match a real title. Matching is now literal with the
//! same boundary set; these pin both the fix and the existing behaviour.

use kobload_api::quality::parse_quality;

#[test]
fn hdr10_plus_is_detected() {
    let p = parse_quality("Show.S01E01.2160p.WEB-DL.HDR10+.HEVC-GRP");
    assert!(p.is_hdr10plus);
    assert!(!p.is_hdr10, "HDR10+ must not also count as plain HDR10");
    assert!(p.is_hdr);
}

#[test]
fn plain_hdr10_is_not_hdr10_plus() {
    let p = parse_quality("Show.S01E01.2160p.WEB-DL.HDR10.HEVC-GRP");
    assert!(p.is_hdr10);
    assert!(!p.is_hdr10plus);
}

#[test]
fn plus_streaming_services_are_detected() {
    let p = parse_quality("Show S01E01 1080p Disney+ WEB-DL DDP5.1 H.264-GRP");
    assert_eq!(p.streaming_service.as_deref(), Some("disney+"));

    let p = parse_quality("Show.S01E01.1080p.Paramount+.WEB-DL-GRP");
    assert_eq!(p.streaming_service.as_deref(), Some("paramount+"));

    let p = parse_quality("Show.S01E01.1080p.AppleTV+.WEB-DL-GRP");
    assert_eq!(p.streaming_service.as_deref(), Some("appletv"));
}

#[test]
fn dotted_tokens_still_match() {
    let p = parse_quality("Movie.2024.2160p.UHD.BluRay.Dolby.Vision.x265-GRP");
    assert!(p.is_dolby_vision);

    let p = parse_quality("Movie.2024.1080p.BluRay.SYNC.FIX.x264-GRP");
    assert!(p.is_sync_fix);
}

#[test]
fn tokens_need_boundaries() {
    // "web" inside a word is not a source; "dv" inside a word is not Dolby Vision.
    let p = parse_quality("Webster.S01E01.Advent.720p.HDTV.x264-GRP");
    assert_eq!(p.source.as_deref(), Some("hdtv"));
    assert!(!p.is_dolby_vision);
}

#[test]
fn bracket_and_space_boundaries() {
    let p = parse_quality("[GRP] Show - 01 [1080p][WEB-DL][HDR10+]");
    assert!(p.is_hdr10plus);
    assert_eq!(p.source.as_deref(), Some("web-dl"));

    let p = parse_quality("Show (2024) 1080p WEBRip REPACK");
    assert!(p.is_repack);
}

#[test]
fn token_at_start_and_end_of_title() {
    let p = parse_quality("PROPER.Show.S01E01.720p.HDTV");
    assert!(p.is_proper);
    assert_eq!(p.source.as_deref(), Some("hdtv"));
}
