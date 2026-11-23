//! A prewarm STATs 64 articles of the file it imported before answering, so a damaged
//! release is demoted and the walk moves on before a viewer ever taps it.

use std::time::{Duration, Instant};

use reqwest::StatusCode;
use serde_json::{Value, json};
use usenet_proto::HealthSample;

use crate::common::usenet::{HealthCall, UsenetRig};
use nats_jobs::types::{StreamOption, StreamPlayback};

const BUDGET: Duration = Duration::from_secs(2);

// Stream searches cache indexer results per show for the whole test process.
fn show(guid: &str) -> String {
    format!("Quick Check {}", guid.replace('-', " "))
}

fn title(guid: &str, quality: &str) -> String {
    format!("{}.S01E05.{quality}.x264-NZB", show(guid).replace(' ', "."))
}

fn warm(option: &StreamOption) -> Value {
    json!({
        "nzb_ref": option.nzb_ref,
        "release_key": option.release_key,
        "file_id": option.files.first().map(|f| f.id),
        "release_title": option.release_title,
        "priority": "warm",
    })
}

async fn failure_reasons(rig: &UsenetRig, key: &str) -> Vec<(Option<i32>, String)> {
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

async fn one_release(guid: &str) -> (UsenetRig, StreamOption) {
    let rig = UsenetRig::start().await;
    rig.tv
        .indexer
        .set_search_results(vec![rig.nzb(guid, &title(guid, "1080p.WEB-DL"))]);
    let option = rig.options(&show(guid), "5").await.remove(0);
    (rig, option)
}

fn quick(guid: &str) -> HealthCall {
    HealthCall {
        guid: guid.to_string(),
        percent: None,
        articles: Some(64),
        output: Some(0),
    }
}

#[tokio::test]
async fn a_damaged_release_fails_its_warm_resolve_and_the_walk_moves_on() {
    let rig = UsenetRig::start().await;
    rig.engine.health(
        "g-damaged",
        None,
        HealthSample {
            checked: 64,
            missing: 2,
        },
    );
    rig.tv.indexer.set_search_results(vec![
        rig.nzb("g-damaged", &title("g-damaged", "2160p.WEB-DL")),
        rig.nzb("g-next", &title("g-damaged", "1080p.WEB-DL")),
    ]);
    let options = rig.options(&show("g-damaged"), "5").await;
    assert_eq!(options[0].nzb_ref, Some(rig.nzb_ref("g-damaged")));

    let (status, body) = rig.resolve(warm(&options[0])).await;

    assert!(!status.is_success(), "{body}");
    let message = body.as_str().unwrap();
    assert!(
        message.contains("Usenet release is not cached") && message.contains("(missing_articles)"),
        "kobbler classifies this text as a verdict on the release: {message}"
    );
    assert_eq!(rig.engine.quick_checks(), vec![quick("g-damaged")]);
    let key: String = sqlx::query_scalar(
        "SELECT release_key FROM usenet_releases WHERE release_key IS NOT NULL LIMIT 1",
    )
    .fetch_one(&rig.tv.server.pool)
    .await
    .unwrap();
    assert_eq!(
        failure_reasons(&rig, &key).await,
        vec![(Some(0), "missing_articles".to_string())]
    );

    let options = rig.options(&show("g-damaged"), "5").await;
    let damaged = options
        .iter()
        .find(|o| o.nzb_ref == Some(rig.nzb_ref("g-damaged")))
        .unwrap();
    assert_eq!(damaged.playback, Some(StreamPlayback::Failed));
    assert_eq!(options[0].nzb_ref, Some(rig.nzb_ref("g-next")));
    let (status, body) = rig.resolve(warm(&options[0])).await;
    assert_eq!(status, StatusCode::OK, "the next option warms: {body}");
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn a_check_that_cannot_answer_never_fails_the_warm_resolve() {
    enum Trouble {
        None,
        Slow,
        Refused,
    }
    for (guid, trouble) in [
        ("g-clean", Trouble::None),
        ("g-slow", Trouble::Slow),
        ("g-error", Trouble::Refused),
    ] {
        let (rig, option) = one_release(guid).await;
        match trouble {
            Trouble::None => {}
            Trouble::Slow => rig.engine.quick_check_delay(BUDGET * 4),
            Trouble::Refused => rig
                .engine
                .quick_check_status(axum::http::StatusCode::INTERNAL_SERVER_ERROR),
        }

        let started = Instant::now();
        let (status, body) = rig.resolve(warm(&option)).await;

        assert_eq!(status, StatusCode::OK, "{guid}: {body}");
        assert!(
            started.elapsed() < BUDGET + Duration::from_secs(1),
            "{guid}: took {:?}",
            started.elapsed()
        );
        assert_eq!(rig.engine.quick_checks(), vec![quick(guid)], "{guid}");
        let key = body["release_key"].as_str().unwrap();
        assert!(failure_reasons(&rig, key).await.is_empty(), "{guid}");
        rig.tv.server.cleanup().await;
    }
}

#[tokio::test]
async fn a_user_resolve_neither_runs_nor_waits_for_the_check() {
    let (rig, option) = one_release("g-user").await;
    rig.engine.quick_check_delay(BUDGET * 4);
    rig.engine.health(
        "g-user",
        None,
        HealthSample {
            checked: 64,
            missing: 9,
        },
    );

    let started = Instant::now();
    let (status, body) = rig.resolve(json!({ "nzb_ref": option.nzb_ref })).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(started.elapsed() < BUDGET, "{:?}", started.elapsed());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(rig.engine.quick_checks().is_empty());
    assert!(
        rig.engine
            .health_calls()
            .iter()
            .any(|c| c.percent == Some(5)),
        "the background 5% sample still runs"
    );
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn a_warm_resolve_that_may_not_grab_runs_no_check() {
    let rig = UsenetRig::start().await;
    rig.tv.indexer.set_search_results(vec![
        rig.nzb("g-pick", &title("g-pick", "1080p.WEB-DL")),
        rig.nzb("g-fallback", &title("g-pick", "480p.HDTV")),
    ]);
    let options = rig.options(&show("g-pick"), "5").await;
    let fallback = options
        .iter()
        .find(|o| o.nzb_ref == Some(rig.nzb_ref("g-fallback")))
        .unwrap();

    let (status, body) = rig.resolve(warm(fallback)).await;

    assert!(!status.is_success(), "{body}");
    assert_eq!(rig.nzbs.fetches("g-fallback"), 0);
    assert!(rig.engine.quick_checks().is_empty());
    rig.tv.server.cleanup().await;
}
