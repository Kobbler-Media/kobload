use kobload_api::quality::{parse_quality, score_quality};
use nats_jobs::QualityOverrides;

const FRENCH_DUBS: &[&str] = &[
    "MotoGP.La.Course.GP.Aragon.30.08.2026.VFF.1080p.WEBRip.AAC.2.0.x264-DN55.mp4",
    "Show.S01E01.VFQ.1080p.WEB.H264-GRP",
    "Movie.2024.VFI.2160p.WEB-DL.DDP5.1.HEVC-GRP",
    "Movie.2024.VF2.1080p.BluRay.x264-GRP",
];

#[test]
fn french_dub_tags_name_the_release_language() {
    for title in FRENCH_DUBS {
        assert_eq!(
            parse_quality(title).language.as_deref(),
            Some("french"),
            "{title}"
        );
    }
}

#[test]
fn english_only_profiles_reject_french_dubs() {
    let english = QualityOverrides {
        preferred_languages: Some(vec!["english".into()]),
        ..Default::default()
    };
    for title in FRENCH_DUBS {
        assert!(
            score_quality(title, None, Some(&english), None).rejected,
            "{title}"
        );
    }
}
