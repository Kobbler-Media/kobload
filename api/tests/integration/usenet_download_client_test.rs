//! Library downloads through our own usenet engine (`client_type = "usenet"`): kobload
//! fetches the NZB, hands it to the engine's job API, polls the job like any other client,
//! imports the finished folder and releases the job on every terminal path.

use base64::Engine as _;
use serde_json::json;
use usenet_proto::{JobFailure, JobState};

use crate::common::usenet_jobs::EngineFixture;

#[tokio::test]
async fn a_grab_runs_through_the_engine_and_is_released_after_import() {
    let fx = EngineFixture::start().await;
    fx.add_usenet_client(true).await;

    let id = fx
        .grab("g-e2e", None, json!({ "password": "s3cret" }))
        .await;

    let added = fx.added();
    assert_eq!(added.len(), 1, "one job submitted: {added:?}");
    let job = &added[0];
    assert_eq!(job.id, id.to_string());
    assert_eq!(job.category, "kobload");
    assert_eq!(job.password.as_deref(), Some("s3cret"));
    let nzb = base64::engine::general_purpose::STANDARD
        .decode(&job.nzb_base64)
        .unwrap();
    assert_eq!(nzb, b"<nzb><file subject=\"g-e2e\"/></nzb>");
    let row = fx.row(id).await;
    assert_eq!(row.status, "downloading");
    assert_eq!(row.external_id.as_deref(), Some(job.id.as_str()));

    fx.set(&job.id, |j| {
        j.state = JobState::Downloading;
        j.progress = 0.5;
        j.total_bytes = 1000;
        j.downloaded_bytes = 500;
    });
    fx.poll().await;
    let row = fx.row(id).await;
    assert_eq!(row.status, "downloading");
    assert_eq!(row.progress, Some(0.5));

    fx.set(&job.id, |j| j.state = JobState::Unpacking);
    fx.poll().await;
    assert_eq!(fx.row(id).await.status, "downloading");

    fx.touch("kobload/Job/Job.S01E01.mkv");
    fx.touch("kobload/Job/Subs/Job.S01E01.en.srt");
    let storage = fx.path("kobload/Job").to_string_lossy().into_owned();
    fx.set(&job.id, |j| {
        j.state = JobState::Completed;
        j.progress = 1.0;
        j.storage = Some(storage);
    });
    fx.poll().await;
    let row = fx.row(id).await;
    assert_eq!(row.status, "completed");
    let files = row.files_json.expect("scanned files");
    let mut names: Vec<&str> = files
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["relative_path"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["Job.S01E01.mkv", "Subs/Job.S01E01.en.srt"]);
    assert!(fx.deleted().is_empty(), "nothing released before import");

    let resp = fx
        .server
        .admin_client()
        .post(&format!("/api/v1/downloads/{id}/imported"))
        .await;
    assert_eq!(resp.status, 204);
    assert_eq!(fx.deleted(), vec![(job.id.clone(), true)]);
    assert_eq!(fx.unauthorized(), 0, "every engine call carries the bearer");
    fx.cleanup().await;
}

#[tokio::test]
async fn cancel_and_clear_release_the_engine_job() {
    let fx = EngineFixture::start().await;
    fx.add_usenet_client(true).await;
    let cancelled = fx.grab("g-cancel", None, json!({})).await;
    let cleared = fx.grab("g-clear", None, json!({})).await;

    let resp = fx
        .server
        .admin_client()
        .delete(&format!("/api/v1/downloads/{cancelled}?cancel=true"))
        .await;
    assert_eq!(resp.status, 204);
    assert_eq!(fx.deleted(), vec![(cancelled.to_string(), true)]);

    sqlx::query("UPDATE downloads SET status = 'failed' WHERE id = $1")
        .bind(cleared)
        .execute(&fx.server.pool)
        .await
        .unwrap();
    let resp = fx
        .server
        .admin_client()
        .delete("/api/v1/downloads?cancel_protocols=usenet")
        .await;
    assert_eq!(resp.status, 200);
    assert!(
        fx.deleted().contains(&(cleared.to_string(), true)),
        "{:?}",
        fx.deleted()
    );
    fx.cleanup().await;
}

#[tokio::test]
async fn a_failed_job_maps_to_a_content_or_an_infrastructure_outcome() {
    struct Case {
        failure: Option<JobFailure>,
        prior_attempts: i32,
        status: &'static str,
        attempts: i32,
    }
    let cases = [
        Case {
            failure: Some(JobFailure::Content),
            prior_attempts: 0,
            status: "failed",
            attempts: 0,
        },
        Case {
            failure: None,
            prior_attempts: 0,
            status: "failed",
            attempts: 0,
        },
        Case {
            failure: Some(JobFailure::Infrastructure),
            prior_attempts: 0,
            status: "pending_retry",
            attempts: 1,
        },
        Case {
            failure: Some(JobFailure::Infrastructure),
            prior_attempts: 9,
            status: "failed",
            attempts: 10,
        },
    ];
    let fx = EngineFixture::start().await;
    fx.add_usenet_client(true).await;
    for (i, case) in cases.iter().enumerate() {
        let id = fx.grab(&format!("g-fail-{i}"), None, json!({})).await;
        sqlx::query("UPDATE downloads SET submission_attempts = $1 WHERE id = $2")
            .bind(case.prior_attempts)
            .bind(id)
            .execute(&fx.server.pool)
            .await
            .unwrap();
        fx.set(&id.to_string(), |j| {
            j.state = JobState::Failed;
            j.error = Some("missing articles".to_string());
            j.failure = case.failure;
        });
        fx.poll().await;

        let row = fx.row(id).await;
        let label = format!("{:?} after {} attempts", case.failure, case.prior_attempts);
        assert_eq!(row.status, case.status, "{label}");
        assert_eq!(row.submission_attempts, case.attempts, "{label}");
        assert!(
            fx.deleted().contains(&(id.to_string(), true)),
            "{label}: the failed engine job is released: {:?}",
            fx.deleted()
        );
        if case.status == "pending_retry" {
            assert_eq!(row.external_id, None, "{label}");
            assert_eq!(row.error, None, "{label}: not a failure");
        } else {
            assert!(
                row.error
                    .as_deref()
                    .unwrap_or_default()
                    .contains("missing articles"),
                "{label}: {:?}",
                row.error
            );
        }
    }
    fx.cleanup().await;
}

#[tokio::test]
async fn a_per_grab_client_id_targets_the_engine_while_sab_stays_default() {
    let fx = EngineFixture::start().await;
    let sab = fx
        .add_client(json!({
            "name": "SAB",
            "client_type": "sabnzbd",
            "base_url": "http://127.0.0.1:9",
            "api_key": "key",
            "is_default": true,
        }))
        .await;
    let usenet = fx.add_usenet_client(false).await;

    let default_grab = fx.grab("g-default", None, json!({})).await;
    let targeted = fx.grab("g-targeted", Some(usenet), json!({})).await;

    assert_eq!(fx.row(default_grab).await.download_client_id, Some(sab));
    let row = fx.row(targeted).await;
    assert_eq!(row.download_client_id, Some(usenet));
    assert_eq!(row.status, "downloading");
    let added: Vec<String> = fx.added().into_iter().map(|j| j.id).collect();
    assert_eq!(added, vec![targeted.to_string()]);
    fx.cleanup().await;
}

#[tokio::test]
async fn reconcile_requeues_downloads_the_engine_lost_and_leaves_its_orphans() {
    let fx = EngineFixture::start().await;
    let client = fx.add_usenet_client(true).await;
    let known = fx.in_flight(client, "g-known", "g-known-job").await;
    fx.seed_job("g-known-job", JobState::Downloading);
    let lost = fx.in_flight(client, "g-lost", "lost-job").await;
    fx.seed_job("orphan-job", JobState::Downloading);

    fx.poll().await;

    let added: Vec<String> = fx.added().into_iter().map(|j| j.id).collect();
    assert_eq!(
        added,
        vec![lost.to_string()],
        "only the lost one is resubmitted"
    );
    let row = fx.row(lost).await;
    assert_eq!(row.status, "downloading");
    assert_eq!(row.external_id, Some(lost.to_string()));
    assert_eq!(row.error, None);
    assert_eq!(
        fx.row(known).await.external_id.as_deref(),
        Some("g-known-job")
    );
    assert!(
        fx.deleted().is_empty(),
        "orphans are left alone: {:?}",
        fx.deleted()
    );
    assert!(fx.engine.jobs.lock().unwrap().contains_key("orphan-job"));
    fx.cleanup().await;
}

#[tokio::test]
async fn an_unreachable_engine_is_a_transient_poll_error() {
    let fx = EngineFixture::unreachable().await;
    let client = fx.add_usenet_client(true).await;
    let id = fx.in_flight(client, "g-down", "g-down-job").await;

    fx.poll().await;
    fx.poll().await;

    let row = fx.row(id).await;
    assert_eq!(row.status, "downloading", "not failed: {:?}", row.error);
    assert_eq!(row.external_id.as_deref(), Some("g-down-job"));
    assert_eq!(row.error, None);
    let poll_error = row.last_poll_error.unwrap_or_default();
    assert!(
        poll_error.starts_with("request error"),
        "the transient poll error is recorded: {poll_error:?}"
    );
    fx.cleanup().await;
}

#[tokio::test]
async fn the_client_type_needs_no_url_or_key_and_tests_the_engine_health() {
    let fx = EngineFixture::start().await;
    let types: serde_json::Value = fx
        .server
        .admin_client()
        .get("/api/v1/admin/download-client-types")
        .await
        .json();
    let usenet = types
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "usenet")
        .expect("usenet client type listed");
    assert_eq!(usenet["protocol"], "usenet");
    assert_eq!(usenet["name"], "Usenet (built-in engine)");
    let fields: Vec<&str> = usenet["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(fields, ["category"]);

    let id = fx.add_usenet_client(true).await;
    let result: serde_json::Value = fx
        .server
        .admin_client()
        .post(&format!("/api/v1/admin/download-clients/{id}/test"))
        .await
        .json();
    assert_eq!(result["success"], true, "{result}");
    fx.cleanup().await;
}
