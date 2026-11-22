//! Kobbler failing to fetch or store a download's files (kobload unreachable,
//! disk or DB trouble) says nothing about the release. Such a report must
//! resend the ingest later; it must not exclude the release, re-search, or
//! delete the files — not even once the retries run out.

use crate::common;

use serde_json::json;
use uuid::Uuid;

use common::sab::{SabFixture, snapshot};
use kobload_api::repositories::downloads as downloads_repo;
use kobload_api::services::import_outcome;

#[derive(Clone, Copy, Debug)]
enum Entry {
    Http,
    Nats,
}

const ENTRIES: [Entry; 2] = [Entry::Http, Entry::Nats];

async fn report_retry(fx: &SabFixture, entry: Entry, id: Uuid) {
    let error = "Ingest returned status 502 Bad Gateway";
    match entry {
        Entry::Http => {
            let resp = fx
                .server
                .admin_client()
                .post_json(
                    &format!("/api/v1/downloads/{id}/import-retry"),
                    &json!({ "error": error }),
                )
                .await;
            assert_eq!(resp.status, 204, "{entry:?}");
        }
        Entry::Nats => {
            import_outcome::retry_import(&fx.server.app_state, id, Some(error)).await;
        }
    }
}

async fn seed(fx: &SabFixture, callback_attempts: i32) -> Uuid {
    fx.touch("complete/Job/Job.mkv");
    fx.sab_job("nzo", "Job", "Completed", "complete/Job/Job.mkv");
    let storage = fx.path("complete/Job/Job.mkv");
    let id = fx
        .download("nzo", "completed", snapshot(&storage.to_string_lossy()))
        .await;
    sqlx::query(
        "UPDATE downloads SET callback_url = 'http://127.0.0.1:9/ingest', callback_sent_at = NOW(),
                callback_attempts = $1, callback_last_attempt_at = NOW() - INTERVAL '1 day',
                callback_idempotency_key = $2
         WHERE id = $3",
    )
    .bind(callback_attempts)
    .bind(Uuid::now_v7())
    .bind(id)
    .execute(&fx.server.pool)
    .await
    .unwrap();
    id
}

#[derive(sqlx::FromRow, Debug)]
struct Row {
    status: String,
    callback_pending: bool,
    fresh_key: bool,
    backing_off: bool,
}

async fn row(fx: &SabFixture, id: Uuid) -> Row {
    sqlx::query_as(
        "SELECT status, callback_sent_at IS NULL AS callback_pending,
                callback_idempotency_key IS NULL AS fresh_key,
                callback_last_attempt_at > NOW() - INTERVAL '1 minute' AS backing_off
         FROM downloads WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&fx.server.pool)
    .await
    .unwrap()
}

async fn events(fx: &SabFixture, id: Uuid) -> Vec<String> {
    sqlx::query_scalar("SELECT event_type FROM download_events WHERE download_id = $1")
        .bind(id)
        .fetch_all(&fx.server.pool)
        .await
        .unwrap()
}

fn kept_release(fx: &SabFixture, events: &[String]) -> Vec<String> {
    let mut problems = Vec::new();
    if !fx.path("complete/Job/Job.mkv").exists() {
        problems.push("files deleted".to_string());
    }
    if !fx.sab.deleted.lock().unwrap().is_empty() {
        problems.push("SAB job removed".to_string());
    }
    if events.iter().any(|e| e == "research_triggered") {
        problems.push("release excluded and re-searched".to_string());
    }
    problems
}

#[tokio::test]
async fn infrastructure_failure_resends_the_ingest_after_a_backoff() {
    let mut failures = Vec::new();
    for entry in ENTRIES {
        let fx = SabFixture::start(true).await;
        let id = seed(&fx, 1).await;

        report_retry(&fx, entry, id).await;

        let row = row(&fx, id).await;
        let events = events(&fx, id).await;
        let mut problems = kept_release(&fx, &events);
        if row.status != "completed" {
            problems.push(format!("status {}", row.status));
        }
        if !row.callback_pending {
            problems.push("ingest not queued for resend".to_string());
        }
        if !row.fresh_key {
            problems.push("idempotency key kept, kobbler would drop the resend".to_string());
        }
        if !row.backing_off {
            problems.push("no backoff before the resend".to_string());
        }
        let pending = downloads_repo::fetch_pending_downloads(&fx.server.pool)
            .await
            .unwrap();
        if pending.iter().any(|d| d.id == id) {
            problems.push("resent immediately instead of after the backoff".to_string());
        }
        if !events.iter().any(|e| e == "import_retry_scheduled") {
            problems.push(format!("no import_retry_scheduled event in {events:?}"));
        }
        failures.extend(problems.into_iter().map(|p| format!("{entry:?}: {p}")));
        fx.cleanup().await;
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn exhausted_retries_fail_the_download_but_keep_the_release() {
    let mut failures = Vec::new();
    for entry in ENTRIES {
        let fx = SabFixture::start(true).await;
        let id = seed(&fx, downloads_repo::MAX_CALLBACK_ATTEMPTS).await;

        report_retry(&fx, entry, id).await;

        let row = row(&fx, id).await;
        let events = events(&fx, id).await;
        let mut problems = kept_release(&fx, &events);
        if row.status != "failed" {
            problems.push(format!("status {}, expected failed", row.status));
        }
        if row.callback_pending {
            problems.push("still queued for resend".to_string());
        }
        if !events.iter().any(|e| e == "import_retry_exhausted") {
            problems.push(format!("no import_retry_exhausted event in {events:?}"));
        }

        downloads_repo::reset_callback_tracking(
            &fx.server.pool,
            id,
            "http://127.0.0.1:9/ingest",
            None,
        )
        .await
        .unwrap();
        let (status, attempts): (String, i32) =
            sqlx::query_as("SELECT status, callback_attempts FROM downloads WHERE id = $1")
                .bind(id)
                .fetch_one(&fx.server.pool)
                .await
                .unwrap();
        if status != "completed" || attempts != 0 {
            problems.push(format!(
                "manual reingest left status {status} with {attempts} attempts used"
            ));
        }
        failures.extend(problems.into_iter().map(|p| format!("{entry:?}: {p}")));
        fx.cleanup().await;
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
