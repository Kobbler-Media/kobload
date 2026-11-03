use crate::common;

use common::TestServer;
use common::stream_fixtures::{create_provider, hash, option, unknown};
use kobload_api::services::stream::warm_cache::UnavailableReason;
use kobload_api::services::stream::{
    KnownAvailability, RankCandidate, RankPreferences, RankProvider, outcomes, rank_options,
};
use nats_jobs::types::{StreamAvailability, StreamGroup, StreamOption};
use reqwest::StatusCode;
use serde_json::json;

async fn ranked(
    server: &TestServer,
    provider: &RankProvider,
    candidates: Vec<RankCandidate>,
) -> Vec<StreamOption> {
    rank_options(
        &server.pool,
        provider,
        candidates,
        &RankPreferences::default(),
    )
    .await
}

fn titles(options: &[StreamOption]) -> Vec<&str> {
    options.iter().map(|o| o.release_title.as_str()).collect()
}

fn with(option: StreamOption, availability: KnownAvailability) -> RankCandidate {
    RankCandidate {
        option,
        availability,
    }
}

#[tokio::test]
async fn test_groups_run_played_account_by_quality_uncached_then_failed() {
    let server = TestServer::start().await;
    let p = create_provider(&server.pool, "real_debrid").await;
    let played = "Movie.2024.720p.BluRay.x264-GGG";
    server
        .admin_client()
        .post_json(
            "/api/v1/stream/outcome",
            &json!({ "debrid_provider_id": p.id, "info_hash": hash('g'), "ok": true,
                     "release_title": played }),
        )
        .await
        .assert_status(StatusCode::NO_CONTENT);

    for i in 0..6 {
        outcomes::record_probe(
            &server.pool,
            p.id,
            &format!("{:040x}", 100 + i),
            Some(&format!("Other.{i}.2024.ZQX-G{i}")),
            Some(UnavailableReason::Blocked),
        )
        .await;
    }

    let candidates = vec![
        with(
            option(&p, &hash('a'), "Movie.2024.1080p.BluRay.x264-AAA", 10.0),
            KnownAvailability::LikelyDead,
        ),
        with(
            option(&p, &hash('b'), "Movie.2024.1080p.BluRay.x264-BBB", 9.5),
            KnownAvailability::Dead,
        ),
        with(
            option(&p, &hash('c'), "Movie.2024.1080p.ZQX.x264-CCC", 9.0),
            KnownAvailability::LikelyAvailable,
        ),
        with(
            option(&p, &hash('d'), "Movie.2024.1080p.BluRay.x264-DDD", 8.0),
            KnownAvailability::Unknown,
        ),
        with(
            option(&p, &hash('e'), "Movie.2024.1080p.BluRay.x264-EEE", 7.0),
            KnownAvailability::LikelyAvailable,
        ),
        with(
            option(&p, &hash('f'), "Movie.2024.1080p.BluRay.x264-FFF", 6.0),
            KnownAvailability::Available,
        ),
        with(
            option(&p, &hash('g'), played, 5.0),
            KnownAvailability::Unknown,
        ),
        with(
            option(&p, &hash('h'), "Movie.2024.1080p.BluRay.x264-HHH", 99.0),
            KnownAvailability::Blocked,
        ),
        with(
            option(&p, &hash('i'), "Movie.2024.720p.BluRay.x264-III", 4.0),
            KnownAvailability::InAccount,
        ),
        with(
            option(&p, &hash('j'), "Movie.2024.2160p.BluRay.x265-JJJ", 50.0),
            KnownAvailability::LikelyRefused,
        ),
    ];
    let out = ranked(&server, &p, candidates).await;

    assert_eq!(
        titles(&out),
        vec![
            played,
            "Movie.2024.1080p.BluRay.x264-FFF",
            "Movie.2024.720p.BluRay.x264-III",
            "Movie.2024.1080p.BluRay.x264-EEE",
            "Movie.2024.1080p.BluRay.x264-DDD",
            "Movie.2024.1080p.ZQX.x264-CCC",
            "Movie.2024.2160p.BluRay.x265-JJJ",
            "Movie.2024.1080p.BluRay.x264-AAA",
            "Movie.2024.1080p.BluRay.x264-HHH",
            "Movie.2024.1080p.BluRay.x264-BBB",
        ],
        "played > on account (held or probed ready) > learned refusal risk, then Torrentio RD+ \
         over unknown > uncached, likely refused included > failed"
    );
    use StreamGroup as G;
    assert_eq!(
        out.iter().map(|o| o.group).collect::<Vec<_>>(),
        [
            G::Played,
            G::Account,
            G::Account,
            G::Available,
            G::Available,
            G::Available,
            G::Uncached,
            G::Uncached,
            G::Failed,
            G::Failed,
        ]
        .map(Some)
    );
    use StreamAvailability::*;
    assert_eq!(
        out.iter().map(|o| o.availability).collect::<Vec<_>>(),
        vec![
            Some(Likely),
            Some(Ready),
            Some(Ready),
            Some(Likely),
            Some(Unknown),
            Some(Likely),
            Some(Uncached),
            Some(Uncached),
            Some(Uncached),
            Some(Uncached),
        ]
    );
    assert_eq!(
        out.iter().map(|o| o.cached).collect::<Vec<_>>(),
        out.iter()
            .map(|o| o.availability == Some(Ready))
            .collect::<Vec<_>>(),
        "cached means ready, nothing weaker"
    );
    server.cleanup().await;
}

#[tokio::test]
async fn test_quality_tiebreaks_within_a_tier() {
    let server = TestServer::start().await;
    let p = create_provider(&server.pool, "torbox").await;
    let tiny = |mut o: StreamOption| {
        o.size = 40_000_000;
        o
    };
    let cases: Vec<([StreamOption; 2], &str, &str)> = vec![
        (
            [
                tiny(option(
                    &p,
                    &hash('a'),
                    "Movie.2024.1080p.BluRay.x264-AAA",
                    9.0,
                )),
                option(&p, &hash('b'), "Movie.2024.1080p.BluRay.x264-BBB", 5.0),
            ],
            "Movie.2024.1080p.BluRay.x264-BBB",
            "a 40MB 1080p movie is a fake or a sample",
        ),
        (
            [
                option(&p, &hash('a'), "Movie.2024.1080p.BluRay.AV1-AAA", 9.5),
                option(&p, &hash('b'), "Movie.2024.1080p.BluRay.x264-BBB", 9.0),
            ],
            "Movie.2024.1080p.BluRay.x264-BBB",
            "near-equal scores go to the codec every device decodes",
        ),
        (
            [
                option(&p, &hash('a'), "Movie.2024.2160p.BluRay.AV1-AAA", 9.0),
                option(&p, &hash('b'), "Movie.2024.720p.BluRay.x264-BBB", 3.0),
            ],
            "Movie.2024.2160p.BluRay.AV1-AAA",
            "compatibility only breaks near-ties",
        ),
        (
            [
                option(&p, &hash('a'), "Movie.2024.2160p.BluRay.DV.HEVC-AAA", 9.5),
                option(
                    &p,
                    &hash('b'),
                    "Movie.2024.2160p.BluRay.HDR10.HEVC-BBB",
                    9.0,
                ),
            ],
            "Movie.2024.2160p.BluRay.HDR10.HEVC-BBB",
            "Dolby Vision without an HDR10 base renders wrong on non-DV screens",
        ),
    ];
    for (options, want, why) in cases {
        assert_eq!(
            titles(&ranked(&server, &p, unknown(&options)).await)[0],
            want,
            "{why}"
        );
    }
    server.cleanup().await;
}
