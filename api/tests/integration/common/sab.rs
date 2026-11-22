use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use uuid::Uuid;

use super::TestServer;
use kobload_api::repositories::downloads as downloads_repo;

#[derive(Default)]
pub struct MockSab {
    pub history: Mutex<Vec<Value>>,
    pub deleted: Mutex<Vec<String>>,
}

async fn sab_api(
    State(sab): State<Arc<MockSab>>,
    Query(q): Query<HashMap<String, String>>,
) -> Json<Value> {
    let mode = q.get("mode").map(String::as_str).unwrap_or_default();
    if q.get("name").map(String::as_str) == Some("delete") {
        let id = q.get("value").cloned().unwrap_or_default();
        if mode == "history" {
            sab.history.lock().unwrap().retain(|s| s["nzo_id"] != id);
            sab.deleted.lock().unwrap().push(id);
        }
        return Json(json!({ "status": true }));
    }
    match mode {
        "queue" => Json(json!({ "queue": { "slots": [] } })),
        "history" => Json(json!({ "history": { "slots": *sab.history.lock().unwrap() } })),
        _ => Json(json!({})),
    }
}

pub struct SabFixture {
    pub server: TestServer,
    pub sab: Arc<MockSab>,
    pub root: tempfile::TempDir,
    pub client_id: Uuid,
}

impl SabFixture {
    pub async fn start(delete_after_import: bool) -> Self {
        let sab = Arc::new(MockSab::default());
        let app = Router::new()
            .route("/api", get(sab_api))
            .with_state(sab.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let sab_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });

        let server = TestServer::start().await;
        let client_id: Uuid = sqlx::query_scalar(
            "INSERT INTO download_clients (id, name, client_type, protocol, base_url, api_key, enabled, is_default, delete_after_import)
             VALUES ($1, 'SAB', 'sabnzbd', 'usenet', $2, 'key', true, true, $3) RETURNING id",
        )
        .bind(Uuid::now_v7())
        .bind(&sab_url)
        .bind(delete_after_import)
        .fetch_one(&server.pool)
        .await
        .unwrap();

        let root = tempfile::tempdir().unwrap();
        Self {
            server,
            sab,
            root,
            client_id,
        }
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    pub fn touch(&self, rel: &str) {
        let path = self.path(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
    }

    pub fn sab_job(&self, nzo_id: &str, name: &str, status: &str, storage: &str) {
        self.sab.history.lock().unwrap().push(json!({
            "nzo_id": nzo_id,
            "name": name,
            "status": status,
            "storage": self.path(storage).to_string_lossy(),
            "fail_message": if status == "Failed" { "Aborted" } else { "" },
            "bytes": 1,
        }));
    }

    /// `client_data` is the poll-time snapshot kobload stored; production
    /// showed SAB reporting `storage: ""` there while post-processing finished.
    pub async fn download(&self, external_id: &str, status: &str, client_data: Value) -> Uuid {
        let id = downloads_repo::create_download(
            &self.server.pool,
            downloads_repo::DownloadInput {
                source: "test".to_string(),
                protocol: "usenet".to_string(),
                name: "Show.S01E01.1080p.WEB-DL-GRP".to_string(),
                download_url: "http://example.com/x.nzb".to_string(),
                download_client_id: self.client_id,
                external_id: Some(external_id.to_string()),
                callback_url: Some(String::new()),
                metadata: Some(json!({})),
                status: status.to_string(),
                media_kind: Some("tv".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        sqlx::query("UPDATE downloads SET client_data = $1 WHERE id = $2")
            .bind(client_data)
            .bind(id)
            .execute(&self.server.pool)
            .await
            .unwrap();
        id
    }

    pub fn release_failures(
        &self,
        case: &str,
        nzo_ids: &[&str],
        gone: &[&str],
        kept: &[&str],
    ) -> Vec<String> {
        let deleted = self.sab.deleted.lock().unwrap().clone();
        let gone = gone
            .iter()
            .filter(|rel| self.path(rel).exists())
            .map(|rel| format!("{case}: {rel} should have been deleted"));
        let kept = kept
            .iter()
            .filter(|rel| !self.path(rel).exists())
            .map(|rel| format!("{case}: {rel} must survive"));
        let sab = nzo_ids
            .iter()
            .filter(|id| !deleted.iter().any(|d| d == *id))
            .map(|id| format!("{case}: SAB history entry {id} not deleted"));
        gone.chain(kept).chain(sab).collect()
    }

    pub async fn cleanup(self) {
        self.server.cleanup().await;
    }
}

pub fn snapshot(storage: &str) -> Value {
    json!({ "nzo_id": "nzo", "status": "Completed", "storage": storage })
}
