use crate::common;

use common::TestServer;
use common::stream_fixtures::{create_provider, hash, option, with_file};
use kobload_api::services::stream::{
    KnownAvailability, RankCandidate, RankPreferences, RankProvider, rank_options,
};
use nats_jobs::types::{StreamAvailability, StreamOption, StreamPreference};
use reqwest::StatusCode;
use serde_json::json;

const PACK: &str = "Show.S01.1080p.WEB-DL.DDP5.1.x264-PACK";

async fn rank(
    server: &TestServer,
    provider: &RankProvider,
    options: Vec<(StreamOption, KnownAvailability)>,
    prefer: &StreamPreference,
) -> Vec<StreamOption> {
    let candidates = options
        .into_iter()
        .map(|(option, availability)| RankCandidate {
            option,
            availability,
        })
        .collect();
    let preferences = RankPreferences {
        prefer: Some(prefer),
        ..Default::default()
    };
    rank_options(&server.pool, provider, candidates, &preferences).await
}

async fn report(server: &TestServer, provider: &RankProvider, h: &str, file_id: i32, ok: bool) {
    server
        .admin_client()
        .post_json(
            "/api/v1/stream/outcome",
            &json!({ "debrid_provider_id": provider.id, "info_hash": h, "file_id": file_id,
                     "ok": ok, "reason": "decoder_error" }),
        )
        .await
        .assert_status(StatusCode::NO_CONTENT);
}

fn previous_from_pack() -> StreamPreference {
    StreamPreference {
        info_hash: hash('p'),
        file_id: Some(0),
        release_title: Some(PACK.to_string()),
    }
}

fn episode_two(provider: &RankProvider) -> Vec<(StreamOption, KnownAvailability)> {
    vec![
        (
            with_file(
                option(provider, &hash('r'), "Show.S01E02.2160p.WEB-DL-READY", 9.0),
                1,
            ),
            KnownAvailability::Available,
        ),
        (
            with_file(
                option(provider, &hash('w'), "Show.S01E02.1080p.WEB-DL-PLAYED", 8.0),
                1,
            ),
            KnownAvailability::Unknown,
        ),
        (
            with_file(option(provider, &hash('p'), PACK, 2.0), 1),
            KnownAvailability::Unknown,
        ),
    ]
}

#[tokio::test]
async fn test_next_episode_of_the_previous_pack_is_ready_and_leads() {
    let server = TestServer::start().await;
    let p = create_provider(&server.pool, "real_debrid").await;
    report(&server, &p, &hash('w'), 1, true).await;
    report(&server, &p, &hash('p'), 0, true).await;

    let ranked = rank(&server, &p, episode_two(&p), &previous_from_pack()).await;
    assert_eq!(ranked[0].release_title, PACK);
    assert_eq!(ranked[0].availability, Some(StreamAvailability::Ready));
    assert!(ranked[0].cached);
    server.cleanup().await;
}

#[tokio::test]
async fn test_a_pack_whose_previous_episode_failed_gets_no_lift() {
    let server = TestServer::start().await;
    let p = create_provider(&server.pool, "real_debrid").await;
    report(&server, &p, &hash('p'), 0, false).await;

    let ranked = rank(&server, &p, episode_two(&p), &previous_from_pack()).await;
    assert_eq!(ranked[0].release_title, "Show.S01E02.2160p.WEB-DL-READY");
    let pack = ranked.iter().find(|o| o.release_title == PACK).unwrap();
    assert_eq!(pack.availability, Some(StreamAvailability::Unknown));
    server.cleanup().await;
}

#[tokio::test]
async fn test_close_scores_keep_the_previous_episodes_look() {
    let server = TestServer::start().await;
    let p = create_provider(&server.pool, "real_debrid").await;
    let prefer = StreamPreference {
        info_hash: hash('q'),
        file_id: None,
        release_title: Some("Show.S01E01.2160p.WEB-DL.DV.HDR10.DDP5.1.Atmos-AAA".to_string()),
    };
    let same_look = "Show.S01E02.2160p.WEB-DL.DV.HDR10.DDP5.1.Atmos-CCC";
    let cases = [
        (10.5, same_look),
        (20.0, "Show.S01E02.1080p.WEB-DL.DDP2.0-BBB"),
    ];
    for (other_score, want) in cases {
        let options = vec![
            (
                option(
                    &p,
                    &hash('b'),
                    "Show.S01E02.1080p.WEB-DL.DDP2.0-BBB",
                    other_score,
                ),
                KnownAvailability::Unknown,
            ),
            (
                option(&p, &hash('c'), same_look, 10.0),
                KnownAvailability::Unknown,
            ),
        ];
        let ranked = rank(&server, &p, options, &prefer).await;
        assert_eq!(ranked[0].release_title, want, "other scores {other_score}");
    }
    server.cleanup().await;
}

/// Both releases played; the replay kobbler asks for is the one played last, and it leads
/// even over a better-scored earlier play.
#[tokio::test]
async fn test_of_two_played_releases_the_preferred_one_leads() {
    let server = TestServer::start().await;
    let p = create_provider(&server.pool, "real_debrid").await;
    let earlier = "Show.S01E01.2160p.WEB-DL-EARLIER";
    let latest = "Show.S01E01.360p.WEB-LATEST";
    for (c, title) in [('e', earlier), ('l', latest)] {
        server
            .admin_client()
            .post_json(
                "/api/v1/stream/outcome",
                &json!({ "debrid_provider_id": p.id, "info_hash": hash(c), "ok": true,
                         "release_title": title }),
            )
            .await
            .assert_status(StatusCode::NO_CONTENT);
    }
    let prefer = StreamPreference {
        info_hash: hash('l'),
        file_id: None,
        release_title: Some(latest.to_string()),
    };
    let out = rank(
        &server,
        &p,
        vec![
            (
                option(&p, &hash('e'), earlier, 9.0),
                KnownAvailability::InAccount,
            ),
            (
                option(&p, &hash('l'), latest, 2.0),
                KnownAvailability::InAccount,
            ),
        ],
        &prefer,
    )
    .await;
    assert_eq!(
        out.iter()
            .map(|o| o.release_title.as_str())
            .collect::<Vec<_>>(),
        [latest, earlier]
    );
    server.cleanup().await;
}
