//! The engine keeps a job, and its folder, until kobload releases it, so every terminal
//! download releases its job whatever `delete_after_import` says; the flag only decides
//! whether an imported download's files go with it.

use serde_json::json;
use usenet_proto::{JobFailure, JobState};

use crate::common::usenet_jobs::EngineFixture;

async fn client(fx: &EngineFixture, delete_after_import: Option<bool>) -> uuid::Uuid {
    let mut body = json!({
        "name": "Usenet",
        "client_type": "usenet",
        "base_url": "",
        "category": "kobload",
        "is_default": true,
    });
    if let Some(flag) = delete_after_import {
        body["delete_after_import"] = json!(flag);
    }
    fx.add_client(body).await
}

async fn delete_after_import(fx: &EngineFixture, id: uuid::Uuid) -> bool {
    sqlx::query_scalar("SELECT delete_after_import FROM download_clients WHERE id = $1")
        .bind(id)
        .fetch_one(&fx.server.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_new_usenet_client_deletes_after_import_by_default() {
    let fx = EngineFixture::start().await;
    let id = client(&fx, None).await;
    assert!(delete_after_import(&fx, id).await);
    fx.cleanup().await;
}

#[tokio::test]
async fn an_imported_download_releases_its_job_and_keeps_files_when_asked() {
    let fx = EngineFixture::start().await;
    client(&fx, Some(false)).await;
    let id = fx.grab("g-keep", None, json!({})).await;
    fx.touch("kobload/Keep/Keep.S01E01.mkv");
    let storage = fx.path("kobload/Keep").to_string_lossy().into_owned();
    fx.set(&id.to_string(), |j| {
        j.state = JobState::Completed;
        j.progress = 1.0;
        j.storage = Some(storage);
    });
    fx.poll().await;
    assert_eq!(fx.row(id).await.status, "completed");

    let resp = fx
        .server
        .admin_client()
        .post(&format!("/api/v1/downloads/{id}/imported"))
        .await;
    assert_eq!(resp.status, 204);
    assert_eq!(fx.deleted(), vec![(id.to_string(), false)]);
    fx.cleanup().await;
}

#[tokio::test]
async fn a_failed_download_always_releases_its_job_with_its_files() {
    let fx = EngineFixture::start().await;
    client(&fx, Some(false)).await;
    let id = fx.grab("g-broken", None, json!({})).await;
    fx.set(&id.to_string(), |j| {
        j.state = JobState::Failed;
        j.error = Some("missing articles".to_string());
        j.failure = Some(JobFailure::Content);
    });
    fx.poll().await;

    assert_eq!(fx.row(id).await.status, "failed");
    assert_eq!(fx.deleted(), vec![(id.to_string(), true)]);
    fx.cleanup().await;
}
