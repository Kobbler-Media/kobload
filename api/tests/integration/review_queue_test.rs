//! The review queue: a want past its date with nothing grabbed shows the
//! releases seen since that look like it, a grab another want would have
//! taken as readily or with a name the matcher could not place is flagged,
//! and the user's answer to either never brings the item back.

use serde_json::{Value, json};
use time::macros::datetime;
use uuid::Uuid;

use crate::common::sport::motogp_race_want;
use crate::common::tv_watch::{TvWatch, episode_want, usenet_release};
use kobload_api::repositories::watch as watch_repo;
use kobload_api::review::refresh;
use kobload_api::watch::{Release, process_releases};

const NOW: time::OffsetDateTime = datetime!(2026-10-05 18:00 UTC);

fn seen(title: &str) -> Release {
    let mut release = usenet_release(title, title);
    release.publish_date = "2026-10-05T02:00:00Z".to_string();
    release
}

async fn items(tv: &TvWatch) -> Vec<Value> {
    let response = tv.server.admin_client().get("/api/v1/review").await;
    response.assert_ok();
    response.json::<Value>()["items"]
        .as_array()
        .unwrap()
        .clone()
}

async fn open_count(tv: &TvWatch) -> i64 {
    let response = tv.server.admin_client().get("/api/v1/review/count").await;
    response.assert_ok();
    response.json::<Value>()["open"].as_i64().unwrap()
}

async fn act(tv: &TvWatch, item: &Value, action: &str, body: Value) -> Value {
    let id = item["id"].as_str().unwrap();
    let response = tv
        .server
        .admin_client()
        .post_json(&format!("/api/v1/review/{id}/{action}"), &body)
        .await;
    assert!(
        response.status.is_success(),
        "{action}: {}",
        response.text()
    );
    serde_json::from_str(&response.text()).unwrap_or(Value::Null)
}

fn titles(item: &Value) -> Vec<&str> {
    item["detail"]["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["title"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn a_missed_race_lists_what_was_seen_and_grabs_it_as_sport() {
    let tv = TvWatch::start().await;
    let state = &tv.server.app_state;
    let pattern = tv.pattern(&motogp_race_want()).await;
    process_releases(
        state,
        &tv.source,
        &[
            seen("MotoGP.2026.Nippon.Race.1080p.WEB.h264-VERUM"),
            seen("Moto2.2026.Japan.Race.1080p.WEB.h264-VERUM"),
            seen("Some.Show.S01E02.1080p.WEB.h264-GRP"),
        ],
        &[],
    )
    .await;

    let stats = refresh(state, NOW).await.unwrap();
    assert_eq!(stats.missed, 1);
    assert_eq!(refresh(state, NOW).await.unwrap().missed, 0, "idempotent");
    assert_eq!(open_count(&tv).await, 1);

    let item = &items(&tv).await[0];
    assert_eq!(item["kind"], "missed");
    assert_eq!(
        titles(item),
        ["MotoGP.2026.Nippon.Race.1080p.WEB.h264-VERUM"],
        "the other class is a different event"
    );
    let candidate = &item["detail"]["candidates"][0];
    assert_eq!(candidate["suggested_alias"], "nippon");
    assert_eq!(candidate["explanation"]["unexplained"], json!(["nippon"]));
    let scopes: Vec<&str> = item["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["scope"].as_str().unwrap())
        .collect();
    assert_eq!(scopes, ["sport_want", "sport_league", "sport_location"]);

    let alias = act(
        &tv,
        item,
        "remember",
        json!({"scope": "sport_location", "alias": "Nippon"}),
    )
    .await;
    assert_eq!(alias["scope_label"], "Japan");
    assert_eq!(open_count(&tv).await, 1, "remembering leaves the item open");

    let grabbed = act(
        &tv,
        item,
        "grab",
        json!({"release_key": "MotoGP.2026.Nippon.Race.1080p.WEB.h264-VERUM"}),
    )
    .await;
    let download_id: Uuid = grabbed["download_id"].as_str().unwrap().parse().unwrap();
    let (media_kind,): (Option<String>,) =
        sqlx::query_as("SELECT media_kind FROM downloads WHERE id = $1")
            .bind(download_id)
            .fetch_one(&tv.server.pool)
            .await
            .unwrap();
    assert_eq!(media_kind.as_deref(), Some("sports"));
    assert_eq!(
        tv.grabbed(pattern.id).await,
        ["MotoGP.2026.Nippon.Race.1080p.WEB.h264-VERUM"]
    );
    assert_eq!(open_count(&tv).await, 0);
    tv.server.cleanup().await;
}

#[tokio::test]
async fn not_it_drops_candidates_until_none_are_left_and_they_never_return() {
    let tv = TvWatch::start().await;
    let state = &tv.server.app_state;
    let mut want = episode_want("Kitsune Days", "1", "13", Some(818_181));
    if let nats_jobs::MediaWant::TvEpisode { air_date, .. } = &mut want {
        *air_date = Some("2026-10-04".to_string());
    }
    tv.pattern(&want).await;
    process_releases(
        state,
        &tv.source,
        &[
            seen("Kitsune.Days.S01E14.1080p.WEB-DL.x264-GRP"),
            seen("Kitsune.Days.S02E13.1080p.WEB-DL.x264-GRP"),
        ],
        &[],
    )
    .await;
    refresh(state, NOW).await.unwrap();
    let item = items(&tv).await.remove(0);
    assert_eq!(titles(&item).len(), 2);

    act(
        &tv,
        &item,
        "not-it",
        json!({"release_key": "Kitsune.Days.S01E14.1080p.WEB-DL.x264-GRP"}),
    )
    .await;
    assert_eq!(
        titles(&items(&tv).await[0]),
        ["Kitsune.Days.S02E13.1080p.WEB-DL.x264-GRP"]
    );
    act(
        &tv,
        &item,
        "not-it",
        json!({"release_key": "Kitsune.Days.S02E13.1080p.WEB-DL.x264-GRP"}),
    )
    .await;
    assert_eq!(open_count(&tv).await, 0);

    assert_eq!(refresh(state, NOW).await.unwrap().missed, 0);
    assert_eq!(open_count(&tv).await, 0);
    tv.server.cleanup().await;
}

async fn grab(tv: &TvWatch, pattern_id: Uuid, title: &str, score: f64) -> Uuid {
    watch_repo::create_match(
        &tv.server.pool,
        &watch_repo::WatchMatchInput {
            pattern_id,
            indexer_source_id: Some(tv.source.id),
            release_guid: title,
            release_title: title,
            release_data: Some(&json!({})),
            match_score: score,
            matched_pattern: "auto",
            action: "downloaded",
            download_id: None,
            hold_until: None,
            held: false,
            quality_score: Some(0.5),
            rejected: false,
            rejected_at: None,
            rejected_reason: None,
            episode_key: None,
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn grabs_a_rival_want_takes_as_readily_or_with_an_unplaced_name_are_doubtful() {
    let tv = TvWatch::start().await;
    let state = &tv.server.app_state;
    let episode = episode_want("Kitsune Days", "1", "13", Some(919_191));
    let first = tv.pattern(&episode).await;
    let rival = tv.pattern(&episode).await;
    let race = tv.pattern(&motogp_race_want()).await;
    let clear = tv
        .pattern(&episode_want("Renard Nights", "2", "1", None))
        .await;

    grab(
        &tv,
        first.id,
        "Kitsune.Days.S01E13.1080p.WEB-DL.x264-GRP",
        0.95,
    )
    .await;
    let race_match = grab(
        &tv,
        race.id,
        "MotoGP.2026.Japan.Race.Mandalika.1080p.WEB.h264-VERUM",
        0.8,
    )
    .await;
    grab(
        &tv,
        clear.id,
        "Renard.Nights.S02E01.1080p.WEB-DL.x264-GRP",
        0.95,
    )
    .await;

    assert_eq!(refresh(state, NOW).await.unwrap().doubtful, 2);
    assert_eq!(refresh(state, NOW).await.unwrap().doubtful, 0, "idempotent");
    let items = items(&tv).await;
    let by_pattern = |id: Uuid| {
        items
            .iter()
            .find(|i| i["watch_pattern_id"] == id.to_string())
            .map(|i| &i["detail"]["grab"])
    };
    assert_eq!(
        by_pattern(first.id).unwrap()["runner_up"]["watch_pattern_id"],
        rival.id.to_string()
    );
    let race_grab = by_pattern(race.id).unwrap();
    assert_eq!(race_grab["unexplained"], json!(["mandalika"]));
    assert_eq!(race_grab["suggested_alias"], "mandalika");
    assert!(by_pattern(clear.id).is_none());

    let race_item = items
        .iter()
        .find(|i| i["watch_pattern_id"] == race.id.to_string())
        .unwrap();
    act(
        &tv,
        race_item,
        "not-it",
        json!({"release_key": "MotoGP.2026.Japan.Race.Mandalika.1080p.WEB.h264-VERUM"}),
    )
    .await;
    let (rejected,): (bool,) = sqlx::query_as("SELECT rejected FROM watch_matches WHERE id = $1")
        .bind(race_match)
        .fetch_one(&tv.server.pool)
        .await
        .unwrap();
    assert!(rejected, "not it rejects the grab");

    let first_item = items
        .iter()
        .find(|i| i["watch_pattern_id"] == first.id.to_string())
        .unwrap();
    act(&tv, first_item, "dismiss", json!({})).await;
    assert_eq!(open_count(&tv).await, 0);
    refresh(state, NOW).await.unwrap();
    assert_eq!(open_count(&tv).await, 0, "answered items never come back");
    tv.server.cleanup().await;
}
