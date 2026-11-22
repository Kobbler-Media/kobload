//! Engine errors that say nothing about the download (a rotated key, a missing engine
//! config, a lost or duplicated job) never fail an in-flight download: the poll retries and
//! reconcile requeues what the engine no longer holds.

use serde_json::json;
use usenet_proto::JobState;

use crate::common::usenet_jobs::EngineFixture;

async fn assert_still_downloading(fx: &EngineFixture, id: uuid::Uuid, label: &str) {
    let row = fx.row(id).await;
    assert_eq!(row.status, "downloading", "{label}: {:?}", row.error);
    assert_eq!(row.error, None, "{label}");
    assert!(
        row.last_poll_error.is_some(),
        "{label}: the poll error is recorded as transient"
    );
}

#[tokio::test]
async fn a_rotated_key_or_a_missing_engine_config_is_a_transient_poll_error() {
    for label in ["rotated key", "missing env"] {
        let fx = match label {
            "rotated key" => EngineFixture::start().await,
            _ => EngineFixture::without_engine().await,
        };
        let client = fx.add_usenet_client(true).await;
        let id = fx.in_flight(client, "g-err", "g-err-job").await;
        fx.seed_job("g-err-job", JobState::Downloading);
        if label == "rotated key" {
            fx.rotate_key();
        }

        fx.poll().await;
        fx.poll().await;

        assert_still_downloading(&fx, id, label).await;
        assert_eq!(
            fx.row(id).await.external_id.as_deref(),
            Some("g-err-job"),
            "{label}"
        );
        let grabbed = fx.grab("g-err-new", None, json!({})).await;
        assert_eq!(
            fx.row(grabbed).await.status,
            "pending_retry",
            "{label}: a new grab waits for the engine instead of failing"
        );
        fx.cleanup().await;
    }
}

#[tokio::test]
async fn an_add_that_timed_out_is_taken_over_when_the_retry_finds_the_job() {
    let fx = EngineFixture::start().await;
    fx.add_usenet_client(true).await;
    fx.time_out_next_add();

    let id = fx.grab("g-timeout", None, json!({})).await;
    let row = fx.row(id).await;
    assert_eq!(row.status, "pending_retry", "{:?}", row.error);
    assert_eq!(row.external_id, None);

    fx.due_for_retry(id).await;
    fx.poll().await;

    let row = fx.row(id).await;
    assert_eq!(row.status, "downloading", "{:?}", row.error);
    assert_eq!(row.external_id, Some(id.to_string()));
    assert_eq!(fx.added().len(), 1, "the retry's 409 adds no second job");
    assert!(fx.deleted().is_empty(), "{:?}", fx.deleted());
    fx.cleanup().await;
}

#[tokio::test]
async fn a_job_lost_between_reconcile_and_poll_is_requeued_not_failed() {
    let fx = EngineFixture::start().await;
    let client = fx.add_usenet_client(true).await;
    let id = fx.in_flight(client, "g-restart", "g-restart-job").await;
    let spent = fx.row(id).await.submission_attempts;
    fx.seed_job("g-restart-job", JobState::Downloading);
    fx.forget_on_get("g-restart-job");

    fx.poll().await;
    assert_still_downloading(&fx, id, "unknown job on poll").await;

    fx.lose("g-restart-job");
    fx.poll().await;

    let row = fx.row(id).await;
    assert_eq!(row.status, "downloading", "{:?}", row.error);
    assert_eq!(row.external_id, Some(id.to_string()), "resubmitted");
    assert_eq!(row.submission_attempts, spent);
    fx.cleanup().await;
}

#[tokio::test]
async fn only_the_first_three_lost_jobs_are_requeued_for_free() {
    let fx = EngineFixture::start().await;
    let client = fx.add_usenet_client(true).await;
    let id = fx.in_flight(client, "g-flaky", "g-flaky-job").await;
    let spent = fx.row(id).await.submission_attempts;

    for lost in 1..=3 {
        fx.poll().await;
        let row = fx.row(id).await;
        assert_eq!(row.status, "downloading", "lost {lost}: {:?}", row.error);
        assert_eq!(row.submission_attempts, spent, "lost {lost}");
        assert_eq!(row.external_id, Some(id.to_string()), "lost {lost}");
        fx.lose(&id.to_string());
    }

    fx.poll().await;
    let row = fx.row(id).await;
    assert_eq!(row.status, "pending_retry", "the fourth loss backs off");
    assert_eq!(row.submission_attempts, spent + 1, "and spends an attempt");
    assert_eq!(row.external_id, None);
    fx.cleanup().await;
}
