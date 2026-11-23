//! kobbler's download-ahead walks to the next option after a permanent refusal, so a
//! refused release must stop being the auto-play pick on the next search. A failure that
//! says nothing about the release must leave it where it was.

use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::common::usenet::{Served, UsenetRig};
use axum::http::StatusCode as EngineStatus;
use nats_jobs::NzbRef;
use nats_jobs::types::{StreamAvailability, StreamOption, StreamPlayback};

const RATE_LIMIT_PAGE: &str =
    r#"<?xml version="1.0"?><error code="500" description="Request limit reached"/>"#;

fn warm(option: &StreamOption) -> Value {
    json!({
        "nzb_ref": option.nzb_ref,
        "release_title": option.release_title,
        "episode": { "season": 1, "episode": 5 },
        "priority": "warm",
    })
}

fn title(show: &str, numbering: &str, quality: &str) -> String {
    format!("{}.{numbering}.{quality}.x264-NZB", show.replace(' ', "."))
}

fn first(options: &[StreamOption]) -> Option<NzbRef> {
    options.first().and_then(|o| o.nzb_ref.clone())
}

fn pack_of<'a>(rig: &UsenetRig, options: &'a [StreamOption]) -> &'a StreamOption {
    let pack = rig.nzb_ref("g-pack");
    options
        .iter()
        .find(|o| o.nzb_ref.as_ref() == Some(&pack))
        .unwrap()
}

fn can_auto_play(option: &StreamOption) -> bool {
    matches!(
        option.availability,
        Some(StreamAvailability::Ready | StreamAvailability::Likely)
    ) && option.playback != Some(StreamPlayback::Failed)
}

async fn failures(rig: &UsenetRig, key: &str) -> Vec<(Option<i32>, String)> {
    sqlx::query_as(
        "SELECT file_id, last_failure_reason FROM stream_release_outcomes
         WHERE release_key = $1 AND provider_id = $2 AND failures > 0",
    )
    .bind(key)
    .bind(rig.provider)
    .fetch_all(&rig.tv.server.pool)
    .await
    .unwrap()
}

async fn imported_key(rig: &UsenetRig, guid: &str) -> String {
    sqlx::query_scalar("SELECT release_key FROM usenet_releases WHERE ref_key = $1")
        .bind(rig.nzb_ref(guid).guid)
        .fetch_one(&rig.tv.server.pool)
        .await
        .unwrap()
}

/// Offers `guid` as the best option with a worse one behind it, fails its warm resolve and
/// answers what the next search would auto-play.
async fn pick_after_failed_warm(rig: &UsenetRig, show: &str, guid: &str) -> Option<NzbRef> {
    let next = format!("{guid}-next");
    rig.tv.indexer.set_search_results(vec![
        rig.nzb(guid, &title(show, "S01E05", "2160p.WEB-DL")),
        rig.nzb(&next, &title(show, "S01E05", "1080p.WEB-DL")),
    ]);
    let options = rig.options(show, "5").await;
    assert_eq!(first(&options), Some(rig.nzb_ref(guid)), "{show}");
    let (status, body) = rig.resolve(warm(&options[0])).await;
    assert!(!status.is_success(), "{show}: {body}");
    first(&rig.options(show, "5").await)
}

#[tokio::test]
async fn a_permanent_nzb_fetch_failure_loses_auto_play_and_a_transient_one_does_not() {
    let cases = [
        (
            "Fetch Gone",
            Some(Served::Status(StatusCode::NOT_FOUND)),
            false,
            true,
        ),
        (
            "Fetch Removed",
            Some(Served::Status(StatusCode::GONE)),
            false,
            true,
        ),
        ("Fetch Huge", Some(Served::Huge), false, true),
        ("Fetch Invalid", None, true, true),
        (
            "Fetch Down",
            Some(Served::Status(StatusCode::INTERNAL_SERVER_ERROR)),
            false,
            false,
        ),
        (
            "Fetch Gateway",
            Some(Served::Status(StatusCode::SERVICE_UNAVAILABLE)),
            false,
            false,
        ),
        (
            "Fetch Busy",
            Some(Served::Status(StatusCode::TOO_MANY_REQUESTS)),
            false,
            false,
        ),
        (
            "Fetch Login",
            Some(Served::Status(StatusCode::UNAUTHORIZED)),
            false,
            false,
        ),
        (
            "Fetch Denied",
            Some(Served::Status(StatusCode::FORBIDDEN)),
            false,
            false,
        ),
        (
            "Fetch Limited",
            Some(Served::Body(RATE_LIMIT_PAGE)),
            true,
            false,
        ),
    ];
    let rig = UsenetRig::start().await;
    for (show, served, engine_rejects, demoted) in cases {
        let guid = format!("g-{}", show.replace(' ', "-").to_lowercase());
        if let Some(served) = served {
            rig.nzbs.serve(&guid, served);
        }
        if engine_rejects {
            let posted = if served.is_some() { "" } else { guid.as_str() };
            rig.engine
                .refuse(posted, EngineStatus::BAD_REQUEST, "invalid_nzb");
        }
        let pick = pick_after_failed_warm(&rig, show, &guid).await;
        let expected = if demoted {
            format!("{guid}-next")
        } else {
            guid.clone()
        };
        assert_eq!(pick, Some(rig.nzb_ref(&expected)), "{show}");
    }
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn a_release_with_no_video_loses_auto_play() {
    let rig = UsenetRig::start().await;
    rig.engine
        .layout("g-docs", &[("Choice.Docs.S01E05.nfo", 9_000)]);

    let pick = pick_after_failed_warm(&rig, "Choice Docs", "g-docs").await;

    assert_eq!(pick, Some(rig.nzb_ref("g-docs-next")));
    let key = imported_key(&rig, "g-docs").await;
    assert_eq!(
        failures(&rig, &key).await,
        vec![(None, "no_video".to_string())]
    );
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn a_pack_without_the_episode_loses_auto_play_for_that_episode_only() {
    let show = "Choice Pack";
    let rig = UsenetRig::start().await;
    rig.engine.layout(
        "g-pack",
        &[
            ("Choice.Pack.S01E01.1080p.mkv", 1_000_000_000),
            ("Choice.Pack.S01E02.1080p.mkv", 1_000_000_000),
        ],
    );
    rig.tv.indexer.set_search_results(vec![
        rig.nzb("g-pack", &title(show, "S01", "2160p.WEB-DL")),
        rig.nzb("g-single", &title(show, "S01E05", "1080p.WEB-DL")),
    ]);
    let before = rig.options(show, "5").await;
    assert!(can_auto_play(pack_of(&rig, &before)), "{before:?}");

    let (status, body) = rig
        .resolve(json!({
            "nzb_ref": pack_of(&rig, &before).nzb_ref,
            "episode": { "season": 1, "episode": 5 },
        }))
        .await;
    assert!(!status.is_success(), "{body}");

    let wanted = rig.options(show, "5").await;
    assert!(!can_auto_play(pack_of(&rig, &wanted)), "{wanted:?}");
    assert_eq!(first(&wanted), Some(rig.nzb_ref("g-single")), "{wanted:?}");
    let key = imported_key(&rig, "g-pack").await;
    assert!(failures(&rig, &key).await.is_empty());

    let other = rig.options(show, "2").await;
    assert_eq!(first(&other), Some(rig.nzb_ref("g-pack")), "{other:?}");
    assert!(can_auto_play(&other[0]), "{other:?}");
    let (status, body) = rig
        .resolve(json!({
            "nzb_ref": other[0].nzb_ref,
            "release_title": other[0].release_title,
            "episode": { "season": 1, "episode": 2 },
            "priority": "warm",
        }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["filename"], "Choice.Pack.S01E02.1080p.mkv");
    rig.tv.server.cleanup().await;
}
