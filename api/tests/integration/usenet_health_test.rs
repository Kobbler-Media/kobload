//! After a usenet stream resolves, kobload asks the engine for a 5% STAT sample of the
//! release in the background. Missing articles the release cannot repair demote it the way
//! a failed playback does; a healthy sample keeps it available.

use std::time::{Duration, Instant};

use reqwest::StatusCode;
use serde_json::json;
use usenet_proto::HealthSample;

use crate::common::usenet::{Served, UsenetRig};

const HEALTH_DELAY: Duration = Duration::from_millis(800);

#[derive(Debug, PartialEq)]
struct Verdict {
    available: bool,
    reason: Option<String>,
    failure: Option<String>,
}

async fn availability(rig: &UsenetRig, key: &str) -> (bool, Option<String>, time::OffsetDateTime) {
    sqlx::query_as(
        "SELECT available, reason, checked_at FROM debrid_availability_cache
         WHERE release_key = $1 AND provider_id = $2",
    )
    .bind(key)
    .bind(rig.provider)
    .fetch_one(&rig.tv.server.pool)
    .await
    .unwrap()
}

async fn failure(rig: &UsenetRig, key: &str) -> Option<String> {
    sqlx::query_scalar(
        "SELECT last_failure_reason FROM stream_release_outcomes
         WHERE release_key = $1 AND provider_id = $2 AND failures > 0",
    )
    .bind(key)
    .bind(rig.provider)
    .fetch_optional(&rig.tv.server.pool)
    .await
    .unwrap()
}

async fn verdict_after_health(rig: &UsenetRig, key: &str) -> Verdict {
    let (_, _, before) = availability(rig, key).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (available, reason, checked_at) = availability(rig, key).await;
        if checked_at > before {
            return Verdict {
                available,
                reason,
                failure: failure(rig, key).await,
            };
        }
        assert!(Instant::now() < deadline, "the health sample never landed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_stream_import_is_demoted_only_when_its_sample_misses_articles_it_cannot_repair() {
    struct Case {
        guid: &'static str,
        par2: bool,
        checked: usize,
        missing: usize,
        demoted: bool,
    }
    let cases = [
        Case {
            guid: "g-healthy",
            par2: false,
            checked: 40,
            missing: 0,
            demoted: false,
        },
        Case {
            guid: "g-holes",
            par2: false,
            checked: 40,
            missing: 1,
            demoted: true,
        },
        Case {
            guid: "g-repairable",
            par2: true,
            checked: 100,
            missing: 1,
            demoted: false,
        },
        Case {
            guid: "g-gutted",
            par2: true,
            checked: 100,
            missing: 10,
            demoted: true,
        },
    ];
    let rig = UsenetRig::start().await;
    rig.engine.health_delay(HEALTH_DELAY);
    for case in &cases {
        if case.par2 {
            rig.nzbs.serve(case.guid, Served::WithPar2);
        }
        rig.engine.health(
            case.guid,
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

        let verdict = verdict_after_health(&rig, key).await;
        let expected = if case.demoted {
            Verdict {
                available: false,
                reason: Some("dead".to_string()),
                failure: Some("missing_articles".to_string()),
            }
        } else {
            Verdict {
                available: true,
                reason: None,
                failure: None,
            }
        };
        assert_eq!(verdict, expected, "{}", case.guid);
    }

    let calls = rig.engine.health_calls();
    let guids: Vec<&str> = calls.iter().map(|(g, _)| g.as_str()).collect();
    assert_eq!(guids, cases.iter().map(|c| c.guid).collect::<Vec<_>>());
    assert!(
        calls.iter().all(|(_, percent)| *percent == Some(5)),
        "{calls:?}"
    );

    let again = rig
        .list("g-healthy", "Health.Show.S01E05.1080p.WEB-DL-g-healthy")
        .await;
    let (status, _) = rig.resolve(json!({ "nzb_ref": again })).await;
    assert_eq!(status, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        rig.engine.health_calls().len(),
        cases.len(),
        "a release sampled recently is not sampled again"
    );
    rig.tv.server.cleanup().await;
}
