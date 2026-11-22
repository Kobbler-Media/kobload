//! Disabling a download client stops new grabs from reaching it, but the jobs it already
//! holds still finish: they are polled to completion, imported and released. The cut-over
//! from SABnzbd to the engine drains SAB this way.

use serde_json::json;
use usenet_proto::JobState;
use uuid::Uuid;

use crate::common::sab::SabFixture;
use crate::common::usenet_jobs::EngineFixture;

async fn disable(pool: &sqlx::PgPool, client_id: Uuid) {
    sqlx::query("UPDATE download_clients SET enabled = FALSE WHERE id = $1")
        .bind(client_id)
        .execute(pool)
        .await
        .unwrap();
}

async fn status(pool: &sqlx::PgPool, id: Uuid) -> String {
    sqlx::query_scalar("SELECT status FROM downloads WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_disabled_sab_client_finishes_and_releases_its_in_flight_jobs() {
    let fx = SabFixture::start(true).await;
    let id = fx.download("nzo", "downloading", json!({})).await;
    disable(&fx.server.pool, fx.client_id).await;
    fx.touch("complete/Job/Job.mkv");
    fx.sab_job("nzo", "Job", "Completed", "complete/Job/Job.mkv");

    kobload_api::polling::poll_once(&fx.server.app_state).await;
    assert_eq!(status(&fx.server.pool, id).await, "completed");

    let resp = fx
        .server
        .admin_client()
        .post(&format!("/api/v1/downloads/{id}/imported"))
        .await;
    assert_eq!(resp.status, 204);
    let failures = fx.release_failures("disabled SAB", &["nzo"], &["complete/Job"], &[]);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    fx.cleanup().await;
}

#[tokio::test]
async fn a_disabled_usenet_client_finishes_and_releases_its_in_flight_jobs() {
    let fx = EngineFixture::start().await;
    let client = fx.add_usenet_client(true).await;
    let id = fx.in_flight(client, "g-drain", "g-drain-job").await;
    fx.seed_job("g-drain-job", JobState::Downloading);
    disable(&fx.server.pool, client).await;

    fx.touch("kobload/Job/Job.mkv");
    let storage = fx.path("kobload/Job").to_string_lossy().into_owned();
    fx.set("g-drain-job", |j| {
        j.state = JobState::Completed;
        j.storage = Some(storage);
    });
    fx.poll().await;
    assert_eq!(status(&fx.server.pool, id).await, "completed");

    let resp = fx
        .server
        .admin_client()
        .post(&format!("/api/v1/downloads/{id}/imported"))
        .await;
    assert_eq!(resp.status, 204);
    assert_eq!(fx.deleted(), vec![("g-drain-job".to_string(), true)]);
    fx.cleanup().await;
}

#[tokio::test]
async fn a_disabled_client_takes_no_new_grabs() {
    let fx = EngineFixture::start().await;
    let client = fx.add_usenet_client(true).await;
    disable(&fx.server.pool, client).await;
    let resp = fx
        .server
        .admin_client()
        .post_json(
            "/api/v1/downloads",
            &json!({
                "source": "test",
                "protocol": "usenet",
                "name": "Nope.S01E01.1080p.WEB-DL-GRP",
                "download_url": fx.nzb_url("g-nope"),
                "download_client_id": client,
                "callback_url": "",
                "metadata": {},
            }),
        )
        .await;
    assert_eq!(resp.status, 400, "{}", resp.text());
    assert!(fx.added().is_empty());
    fx.cleanup().await;
}
