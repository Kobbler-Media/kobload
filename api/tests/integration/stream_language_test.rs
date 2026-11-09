use crate::common;

use common::TestServer;
use common::stream_fixtures::{create_provider, hash, option};
use kobload_api::services::stream::{
    KnownAvailability, RankCandidate, RankPreferences, RankProvider, rank_options,
};
use nats_jobs::types::StreamOption;

type Release = (&'static str, f64, KnownAvailability);

const INCIDENT: &str =
    "Altered.Carbon.1x01.Le.Catene.Della.Colpa.ITA.ENG.1080p.NFRip.AAC.x265-Pir8";

async fn rank(
    server: &TestServer,
    provider: &RankProvider,
    options: &[(StreamOption, KnownAvailability)],
    audio_languages: &[&str],
) -> Vec<StreamOption> {
    let candidates = options
        .iter()
        .cloned()
        .map(|(option, availability)| RankCandidate {
            option,
            availability,
        })
        .collect();
    let audio_languages: Vec<String> = audio_languages.iter().map(|l| l.to_string()).collect();
    let preferences = RankPreferences {
        audio_languages: &audio_languages,
        ..Default::default()
    };
    rank_options(&server.pool, provider, candidates, &preferences).await
}

fn titles(ranked: &[StreamOption]) -> Vec<&str> {
    ranked.iter().map(|o| o.release_title.as_str()).collect()
}

#[tokio::test]
async fn test_audio_language_preference_orders_releases_within_a_tier() {
    let server = TestServer::start().await;
    let p = create_provider(&server.pool, "real_debrid").await;
    let unknown = KnownAvailability::Unknown;
    let cases: &[(&[&str], &[Release], &[&str])] = &[
        (
            &["english"],
            &[
                (INCIDENT, 10.0, unknown),
                (
                    "Altered.Carbon.S01E01.1080p.NF.WEB-DL.DDP5.1.x264-NTb",
                    9.5,
                    unknown,
                ),
            ],
            &[
                "Altered.Carbon.S01E01.1080p.NF.WEB-DL.DDP5.1.x264-NTb",
                INCIDENT,
            ],
        ),
        (
            &["en"],
            &[
                ("Show.S01E01.ITA.1080p.WEB-AAA", 20.0, unknown),
                ("Show.S01E01.ITA.ENG.1080p.WEB-BBB", 5.0, unknown),
                ("Show.S01E01.720p.WEB-CCC", 4.0, unknown),
            ],
            &[
                "Show.S01E01.ITA.ENG.1080p.WEB-BBB",
                "Show.S01E01.720p.WEB-CCC",
                "Show.S01E01.ITA.1080p.WEB-AAA",
            ],
        ),
        (
            &["eng"],
            &[
                (
                    "Show.S01E01.ITA.1080p.WEB-AAA",
                    5.0,
                    KnownAvailability::Available,
                ),
                ("Show.S01E01.1080p.WEB-CCC", 9.0, unknown),
            ],
            &["Show.S01E01.ITA.1080p.WEB-AAA", "Show.S01E01.1080p.WEB-CCC"],
        ),
        (
            &["italian"],
            &[
                ("Show.S01E01.ENG.1080p.WEB-AAA", 9.0, unknown),
                ("Show.S01E01.iTA.1080p.WEB-BBB", 5.0, unknown),
            ],
            &[
                "Show.S01E01.iTA.1080p.WEB-BBB",
                "Show.S01E01.ENG.1080p.WEB-AAA",
            ],
        ),
        (
            &[],
            &[
                ("Show.S01E01.ITA.1080p.WEB-AAA", 9.0, unknown),
                ("Show.S01E01.1080p.WEB-BBB", 5.0, unknown),
            ],
            &["Show.S01E01.ITA.1080p.WEB-AAA", "Show.S01E01.1080p.WEB-BBB"],
        ),
    ];
    for (i, (languages, releases, want)) in cases.iter().enumerate() {
        let options: Vec<_> = releases
            .iter()
            .enumerate()
            .map(|(j, (title, score, availability))| {
                let h = hash(char::from(b'a' + (i * 4 + j) as u8));
                (option(&p, &h, title, *score), *availability)
            })
            .collect();
        let ranked = rank(&server, &p, &options, languages).await;
        assert_eq!(titles(&ranked), *want, "preferring {languages:?}");
    }
    server.cleanup().await;
}

#[tokio::test]
async fn test_listed_audio_languages_are_exposed_as_iso_codes() {
    let server = TestServer::start().await;
    let p = create_provider(&server.pool, "real_debrid").await;
    let options = [
        (
            option(&p, &hash('a'), INCIDENT, 1.0),
            KnownAvailability::Unknown,
        ),
        (
            option(&p, &hash('b'), "Show.S01E01.1080p.WEB-AAA", 1.0),
            KnownAvailability::Unknown,
        ),
    ];
    let ranked = rank(&server, &p, &options, &[]).await;
    let incident = ranked.iter().find(|o| o.release_title == INCIDENT).unwrap();
    assert_eq!(incident.audio_languages, vec!["it", "en"]);
    let plain = ranked.iter().find(|o| o.release_title != INCIDENT).unwrap();
    assert!(plain.audio_languages.is_empty());
    assert!(
        !serde_json::to_string(plain)
            .unwrap()
            .contains("audio_languages")
    );
    server.cleanup().await;
}

/// Subtitles burned into the picture cannot be turned off, so a clean release that can
/// start now wins, wherever it sits; one that would have to be fetched first does not.
#[tokio::test]
async fn test_hardcoded_subtitles_sink_below_clean_releases_that_can_start() {
    let server = TestServer::start().await;
    let p = create_provider(&server.pool, "real_debrid").await;
    let cases: &[(&[Release], &[&str])] = &[
        (
            &[
                (
                    "Nocturne.S01E03.1080p.HDTV.Subtitulado.Esp.SC",
                    20.0,
                    KnownAvailability::InAccount,
                ),
                (
                    "Nocturne.S01E03.Eyes.on.the.Inside.1080p.HEVC.x265-MeGusta",
                    5.0,
                    KnownAvailability::Unknown,
                ),
            ],
            &[
                "Nocturne.S01E03.Eyes.on.the.Inside.1080p.HEVC.x265-MeGusta",
                "Nocturne.S01E03.1080p.HDTV.Subtitulado.Esp.SC",
            ],
        ),
        (
            &[
                (
                    "Show.S01E01.1080p.HC.WEBRip-AAA",
                    20.0,
                    KnownAvailability::Unknown,
                ),
                (
                    "Show.S01E01.1080p.VOSE.WEB-BBB",
                    15.0,
                    KnownAvailability::Unknown,
                ),
                ("Show.S01E01.720p.WEB-CCC", 4.0, KnownAvailability::Unknown),
            ],
            &[
                "Show.S01E01.720p.WEB-CCC",
                "Show.S01E01.1080p.HC.WEBRip-AAA",
                "Show.S01E01.1080p.VOSE.WEB-BBB",
            ],
        ),
        (
            &[
                (
                    "Show.S01E01.1080p.WEB-AAA",
                    20.0,
                    KnownAvailability::LikelyDead,
                ),
                (
                    "Show.S01E01.1080p.HC.WEBRip-BBB",
                    5.0,
                    KnownAvailability::Available,
                ),
            ],
            &[
                "Show.S01E01.1080p.HC.WEBRip-BBB",
                "Show.S01E01.1080p.WEB-AAA",
            ],
        ),
    ];
    for (i, (releases, want)) in cases.iter().enumerate() {
        let options: Vec<_> = releases
            .iter()
            .enumerate()
            .map(|(j, (title, score, availability))| {
                let h = hash(char::from(b'a' + (i * 4 + j) as u8));
                (option(&p, &h, title, *score), *availability)
            })
            .collect();
        let ranked = rank(&server, &p, &options, &["english"]).await;
        assert_eq!(titles(&ranked), *want, "case {i}");
    }
    server.cleanup().await;
}
