use crate::common::tv_watch::{TvWatch, episode_want, usenet_release};

use kobload_api::jobs::handlers::{SearchGates, backlog_search_pattern};

#[tokio::test]
async fn backlog_grabs_the_best_scored_candidate_not_the_first_listed() {
    let tv = TvWatch::start().await;
    tv.indexer.set_search_results(vec![
        usenet_release("hdtv-720p", "Kitsune.Days.S01E13.720p.HDTV.x264-GRP"),
        usenet_release(
            "web-2160p",
            "Kitsune.Days.S01E13.2160p.WEB-DL.DDP5.1.H.265-GRP",
        ),
    ]);
    let pattern = tv
        .pattern(&episode_want("Kitsune Days", "1", "13", None))
        .await;

    backlog_search_pattern(&tv.server.app_state, &pattern, SearchGates::AUTOMATIC).await;

    assert_eq!(tv.grabbed(pattern.id).await, ["web-2160p"]);
    tv.server.cleanup().await;
}
