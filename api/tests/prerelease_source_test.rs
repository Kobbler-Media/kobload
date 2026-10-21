//! Mislabelled pre-release uploads like `The.Odyssey.2026.1080p.TS.V4.WEB.DL`
//! parsed as WEB because source detection was first-match-wins with WEB checked
//! before TS/CAM, so they slipped past every profile's rejected_sources.

use kobload_api::quality::{parse_quality, score_quality};
use kobload_api::repositories::quality_profiles::QualityProfileRow;

const CASES: &[(&str, &str)] = &[
    (
        "The.Odyssey.2026.1080p.TS.V4.WEB.DL-GP-M-NLsubs-Custom",
        "telesync",
    ),
    ("the.odyssey.2026.web.CAM.h264-rbb", "cam"),
    ("Movie.2026.HDTV.TELECINE.x264", "telecine"),
    ("Show.S01E01.720p.HDCAM.WEBRip.x264-GRP", "cam"),
    // A title word is not a source tag.
    ("Cam.2018.1080p.NF.WEB-DL.DDP5.1.x264-NTG", "web-dl"),
    ("The.TS.Movie.2026.1080p.WEB-DL.x264-GRP", "web-dl"),
    // A `.ts` file extension is not a TS source tag.
    ("Show.S01E01.1080p.WEB-DL.x264-GRP.ts", "web-dl"),
];

fn profile() -> QualityProfileRow {
    QualityProfileRow {
        id: uuid::Uuid::new_v4(),
        name: "Movies - 1080p".to_string(),
        media_kind: "movies".to_string(),
        is_default: true,
        min_resolution: None,
        max_resolution: None,
        preferred_sources: vec!["web-dl".to_string()],
        preferred_codecs: vec![],
        preferred_audio: vec![],
        preferred_languages: vec![],
        rejected_languages: vec![],
        rejected_terms: vec![],
        rejected_sources: vec!["cam".to_string(), "ts".to_string(), "tc".to_string()],
        reject_hardcoded_subs: false,
        use_trash_filters: false,
        reject_remux: false,
        upgrade_cutoff_score: None,
        created_at: time::OffsetDateTime::now_utc(),
        updated_at: time::OffsetDateTime::now_utc(),
    }
}

#[test]
fn prerelease_tags_win_over_web_tv_and_dvd_tags() {
    for (title, source) in CASES {
        assert_eq!(
            parse_quality(title).source.as_deref(),
            Some(*source),
            "{title}"
        );
    }
}

#[test]
fn rejected_sources_catch_mislabelled_prereleases() {
    let profile = profile();
    for (title, source) in CASES {
        let scored = score_quality(title, Some(&profile), None, None);
        let prerelease = *source != "web-dl";
        assert_eq!(scored.rejected, prerelease, "{title}");
        if prerelease {
            assert_eq!(
                scored.breakdown.rejected_reason.as_deref(),
                Some("rejected_source"),
                "{title}"
            );
        }
    }
}
