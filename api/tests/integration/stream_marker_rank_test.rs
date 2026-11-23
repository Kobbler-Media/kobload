//! Torrentio's RD+ marker is an availability hint. It lifts a torrent over ones nobody has
//! vouched for, but it must not lift it over a better usenet option on our engine, which is
//! just as "likely".

use crate::common::TestServer;
use crate::common::stream_fixtures::{create_provider, option, usenet_option};
use crate::common::tv_watch::episode_want;
use crate::common::usenet::UsenetRig;
use kobload_api::services::stream::{
    KnownAvailability as A, RankCandidate, RankPreferences, rank_options,
};
use nats_jobs::types::{StreamAvailability, StreamSourceType};

const MARKED_CAM: &str = "Movie.2024.720p.HDCAM.x264-RDPLUS";
const USENET_BLURAY: &str = "Movie.2024.2160p.BluRay.x265-NZB";

#[tokio::test]
async fn a_marked_low_quality_torrent_competes_on_score_with_engine_usenet() {
    let server = TestServer::start().await;
    let p = create_provider(&server.pool, "real_debrid").await;
    let cases = [
        ("better usenet wins", 0.6, 1.8, USENET_BLURAY),
        ("a better marked torrent still wins", 1.9, 1.8, MARKED_CAM),
    ];
    for (label, torrent_score, usenet_score, first) in cases {
        let candidates = vec![
            RankCandidate {
                option: option(&p, &"a".repeat(40), MARKED_CAM, torrent_score),
                availability: A::LikelyAvailable,
            },
            RankCandidate {
                option: usenet_option(&p, "g-nzb", USENET_BLURAY, usenet_score, None),
                availability: A::LikelyAvailable,
            },
        ];
        let out = rank_options(&server.pool, &p, candidates, &RankPreferences::default()).await;
        assert_eq!(out[0].release_title, first, "{label}");
    }
    server.cleanup().await;
}

#[tokio::test]
async fn every_engine_usenet_option_reaches_ranking_as_likely() {
    let rig = UsenetRig::start().await;
    rig.tv.indexer.set_search_results(vec![
        rig.nzb("g-likely", "Marker.Rank.Show.S01E05.1080p.WEB-DL.x264-NZB"),
    ]);
    let options = rig
        .tv
        .stream_options(&episode_want("Marker Rank Show", "1", "5", None))
        .await;
    let usenet: Vec<_> = options
        .iter()
        .filter(|o| o.source_type == StreamSourceType::Usenet)
        .collect();
    assert_eq!(usenet.len(), 1, "{options:?}");
    assert_eq!(usenet[0].availability, Some(StreamAvailability::Likely));
    rig.tv.server.cleanup().await;
}
