//! After a usenet stream resolves, kobload asks the engine for a 5% STAT sample of the file
//! it chose, in the background. A stream cannot repair, so any missing article demotes that
//! file the way a failed playback does; the release's other files keep their standing.

use std::time::{Duration, Instant};

use reqwest::StatusCode;
use serde_json::json;
use usenet_proto::HealthSample;

use crate::common::usenet::{HealthCall, UsenetRig};

const HEALTH_DELAY: Duration = Duration::from_millis(800);
const GB: u64 = 1_000_000_000;

async fn availability(rig: &UsenetRig, key: &str) -> (bool, Option<String>) {
    sqlx::query_as(
        "SELECT available, reason FROM debrid_availability_cache
         WHERE release_key = $1 AND provider_id = $2",
    )
    .bind(key)
    .bind(rig.provider)
    .fetch_one(&rig.tv.server.pool)
    .await
    .unwrap()
}

async fn failures(rig: &UsenetRig, key: &str) -> Vec<(Option<i32>, String)> {
    sqlx::query_as(
        "SELECT file_id, last_failure_reason FROM stream_release_outcomes
         WHERE release_key = $1 AND provider_id = $2 AND failures > 0 ORDER BY file_id",
    )
    .bind(key)
    .bind(rig.provider)
    .fetch_all(&rig.tv.server.pool)
    .await
    .unwrap()
}

async fn sampled(rig: &UsenetRig, calls: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while rig.engine.health_calls().len() < calls {
        assert!(Instant::now() < deadline, "the health sample never ran");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(HEALTH_DELAY + Duration::from_millis(300)).await;
}

fn call(guid: &str, output: usize) -> HealthCall {
    HealthCall {
        guid: guid.to_string(),
        percent: Some(5),
        output: Some(output),
    }
}

#[tokio::test]
async fn any_missing_article_demotes_the_sampled_file_and_nothing_else() {
    struct Case {
        guid: &'static str,
        checked: usize,
        missing: usize,
        demoted: bool,
    }
    let cases = [
        Case {
            guid: "g-healthy",
            checked: 40,
            missing: 0,
            demoted: false,
        },
        Case {
            guid: "g-one-hole",
            checked: 100,
            missing: 1,
            demoted: true,
        },
    ];
    let rig = UsenetRig::start().await;
    rig.engine.health_delay(HEALTH_DELAY);
    for (n, case) in cases.iter().enumerate() {
        rig.engine.health(
            case.guid,
            None,
            HealthSample {
                checked: case.checked,
                missing: case.missing,
            },
        );
        let nzb_ref = rig
            .list(
                case.guid,
                &format!("Health.Show.S01E05.1080p.WEB-DL-{}", case.guid),
            )
            .await;

        let started = Instant::now();
        let (status, resolved) = rig.resolve(json!({ "nzb_ref": nzb_ref })).await;
        assert_eq!(status, StatusCode::OK, "{}: {resolved}", case.guid);
        assert!(
            started.elapsed() < HEALTH_DELAY,
            "{}: the resolve does not wait for the sample",
            case.guid
        );
        let key = resolved["release_key"].as_str().unwrap();
        sampled(&rig, n + 1).await;

        assert_eq!(availability(&rig, key).await, (true, None), "{}", case.guid);
        let expected = if case.demoted {
            vec![(Some(0), "missing_articles".to_string())]
        } else {
            Vec::new()
        };
        assert_eq!(failures(&rig, key).await, expected, "{}", case.guid);
    }

    let calls = rig.engine.health_calls();
    let expected: Vec<HealthCall> = cases.iter().map(|c| call(c.guid, 0)).collect();
    assert_eq!(calls, expected);

    let again = rig
        .list("g-healthy", "Health.Show.S01E05.1080p.WEB-DL-g-healthy")
        .await;
    let (status, _) = rig.resolve(json!({ "nzb_ref": again })).await;
    assert_eq!(status, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        rig.engine.health_calls().len(),
        cases.len(),
        "a file sampled recently is not sampled again"
    );
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn a_damaged_episode_leaves_the_rest_of_its_pack_playable() {
    let rig = UsenetRig::start().await;
    rig.engine.layout(
        "g-pack",
        &[
            ("Health.Pack.S01E01.1080p.mkv", GB),
            ("Health.Pack.S01E02.1080p.mkv", GB),
        ],
    );
    rig.engine.health(
        "g-pack",
        Some(0),
        HealthSample {
            checked: 50,
            missing: 3,
        },
    );
    let nzb_ref = rig.list("g-pack", "Health.Pack.S01.1080p.WEB-DL-GRP").await;

    let episode = |n: u32| json!({ "nzb_ref": nzb_ref, "episode": { "season": 1, "episode": n } });
    let (status, e01) = rig.resolve(episode(1)).await;
    assert_eq!(status, StatusCode::OK, "{e01}");
    let key = e01["release_key"].as_str().unwrap().to_string();
    sampled(&rig, 1).await;
    let (status, e02) = rig.resolve(episode(2)).await;
    assert_eq!(status, StatusCode::OK, "{e02}");
    sampled(&rig, 2).await;

    assert_eq!(
        rig.engine.health_calls(),
        vec![call("g-pack", 0), call("g-pack", 1)],
        "each played file is sampled on its own"
    );
    assert_eq!(
        failures(&rig, &key).await,
        vec![(Some(0), "missing_articles".to_string())],
        "only E01 is demoted"
    );
    assert_eq!(
        availability(&rig, &key).await,
        (true, None),
        "the pack stays available for E02"
    );
    rig.tv.server.cleanup().await;
}
