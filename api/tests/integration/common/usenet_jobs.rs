use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use usenet_proto::{JobRequest, JobState, JobStatus};
use uuid::Uuid;

use super::server::{TestServer, TestServerOptions, local_only_http_client};
use super::usenet::{CONTROL_KEY, INDEXER_KEY, NzbHost, STREAM_BASE, STREAM_SECRET};
use kobload_api::debrid::DefaultDebridProviderResolver;
use kobload_api::download_clients::DefaultDownloadClientResolver;
use kobload_api::repositories::downloads as downloads_repo;
use kobload_api::usenet::{EngineSettings, UsenetEngine};

#[derive(Default)]
pub struct EngineJobs {
    pub jobs: Mutex<HashMap<String, JobStatus>>,
    pub added: Mutex<Vec<JobRequest>>,
    pub deleted: Mutex<Vec<(String, bool)>>,
    pub unauthorized: Mutex<usize>,
    pub poll_status: Mutex<Option<StatusCode>>,
    pub rotated_key: Mutex<bool>,
    pub add_times_out: Mutex<bool>,
    pub restarted: Mutex<HashSet<String>>,
}

impl EngineJobs {
    fn authorized(&self, headers: &HeaderMap) -> bool {
        let ok = !*self.rotated_key.lock().unwrap()
            && headers.get("authorization").and_then(|v| v.to_str().ok())
                == Some(format!("Bearer {CONTROL_KEY}").as_str());
        if !ok {
            *self.unauthorized.lock().unwrap() += 1;
        }
        ok
    }
}

async fn add(
    State(engine): State<Arc<EngineJobs>>,
    headers: HeaderMap,
    Json(req): Json<JobRequest>,
) -> axum::response::Response {
    if !engine.authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if engine.jobs.lock().unwrap().contains_key(&req.id) {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "duplicate_job", "message": "job exists" })),
        )
            .into_response();
    }
    let status = JobStatus {
        id: req.id.clone(),
        name: req.name.clone(),
        state: JobState::Queued,
        progress: 0.0,
        total_bytes: 0,
        downloaded_bytes: 0,
        storage: None,
        error: None,
        failure: None,
    };
    engine
        .jobs
        .lock()
        .unwrap()
        .insert(req.id.clone(), status.clone());
    engine.added.lock().unwrap().push(req);
    if std::mem::take(&mut *engine.add_times_out.lock().unwrap()) {
        return StatusCode::GATEWAY_TIMEOUT.into_response();
    }
    Json(status).into_response()
}

async fn list(
    State(engine): State<Arc<EngineJobs>>,
    headers: HeaderMap,
) -> axum::response::Response {
    if !engine.authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let jobs: Vec<JobStatus> = engine.jobs.lock().unwrap().values().cloned().collect();
    Json(jobs).into_response()
}

async fn status(
    State(engine): State<Arc<EngineJobs>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> axum::response::Response {
    if !engine.authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if let Some(status) = *engine.poll_status.lock().unwrap() {
        return (status, Json(json!({ "error": "providers_unavailable" }))).into_response();
    }
    if engine.restarted.lock().unwrap().contains(&id) {
        return unknown_job();
    }
    match engine.jobs.lock().unwrap().get(&id) {
        Some(job) => Json(job.clone()).into_response(),
        None => unknown_job(),
    }
}

fn unknown_job() -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "unknown_job" })),
    )
        .into_response()
}

async fn delete(
    State(engine): State<Arc<EngineJobs>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> axum::response::Response {
    if !engine.authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let delete_files = q.get("delete_files").map(String::as_str) == Some("true");
    let known = engine.jobs.lock().unwrap().remove(&id).is_some();
    engine.deleted.lock().unwrap().push((id, delete_files));
    if known {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

pub fn engine_settings(url: &str) -> EngineSettings {
    EngineSettings {
        engine_url: url.to_string(),
        stream_base_url: STREAM_BASE.to_string(),
        control_key: CONTROL_KEY.to_string(),
        stream_secret: STREAM_SECRET.to_string(),
    }
}

pub async fn server_with_engine(url: &str) -> TestServer {
    let engine = Arc::new(UsenetEngine::new(
        engine_settings(url),
        local_only_http_client(),
    ));
    let debrid = Arc::new(DefaultDebridProviderResolver::with_usenet(engine));
    let options = TestServerOptions {
        download_clients: Arc::new(DefaultDownloadClientResolver::new(
            sqlx::PgPool::connect_lazy("postgres://dummy").unwrap(),
            local_only_http_client(),
            debrid.clone(),
        )),
        ..TestServerOptions::default()
    };
    TestServer::start_inner(options, debrid).await
}

pub struct EngineFixture {
    pub server: TestServer,
    pub engine: Arc<EngineJobs>,
    pub nzbs: NzbHost,
    pub root: tempfile::TempDir,
}

impl EngineFixture {
    pub async fn start() -> Self {
        let engine = Arc::new(EngineJobs::default());
        let app = Router::new()
            .route("/v1/jobs", get(list).post(add))
            .route("/v1/jobs/{id}", get(status).delete(delete))
            .route("/healthz", get(|| async { "ok" }))
            .with_state(engine.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        Self {
            server: server_with_engine(&url).await,
            engine,
            nzbs: NzbHost::start().await,
            root: tempfile::tempdir().unwrap(),
        }
    }

    pub async fn unreachable() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        Self {
            server: server_with_engine(&url).await,
            engine: Arc::new(EngineJobs::default()),
            nzbs: NzbHost::start().await,
            root: tempfile::tempdir().unwrap(),
        }
    }

    pub async fn without_engine() -> Self {
        Self {
            server: TestServer::start().await,
            engine: Arc::new(EngineJobs::default()),
            nzbs: NzbHost::start().await,
            root: tempfile::tempdir().unwrap(),
        }
    }

    pub fn rotate_key(&self) {
        *self.engine.rotated_key.lock().unwrap() = true;
    }

    pub fn time_out_next_add(&self) {
        *self.engine.add_times_out.lock().unwrap() = true;
    }

    /// The engine restarted after reconcile listed the job and before the poll asked for it.
    pub fn forget_on_get(&self, id: &str) {
        self.engine.restarted.lock().unwrap().insert(id.to_string());
    }

    pub fn lose(&self, id: &str) {
        self.engine.restarted.lock().unwrap().remove(id);
        self.engine.jobs.lock().unwrap().remove(id);
    }

    pub async fn due_for_retry(&self, id: Uuid) {
        sqlx::query(
            "UPDATE downloads SET submission_last_attempt_at = NOW() - INTERVAL '1 day' WHERE id = $1",
        )
        .bind(id)
        .execute(&self.server.pool)
        .await
        .unwrap();
    }

    pub async fn add_client(&self, body: Value) -> Uuid {
        let resp = self
            .server
            .admin_client()
            .post_json("/api/v1/admin/download-clients", &body)
            .await;
        assert_eq!(resp.status, 200, "{}", resp.text());
        resp.json::<Value>()["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap()
    }

    pub async fn add_usenet_client(&self, is_default: bool) -> Uuid {
        self.add_client(json!({
            "name": "Usenet",
            "client_type": "usenet",
            "base_url": "",
            "category": "kobload",
            "is_default": is_default,
        }))
        .await
    }

    pub fn nzb_url(&self, guid: &str) -> String {
        format!("{}/nzb/{guid}?apikey={INDEXER_KEY}", self.nzbs.url)
    }

    pub async fn grab(&self, guid: &str, client_id: Option<Uuid>, release: Value) -> Uuid {
        let resp = self
            .server
            .admin_client()
            .post_json(
                "/api/v1/downloads",
                &json!({
                    "source": "test",
                    "protocol": "usenet",
                    "name": format!("{guid}.S01E01.1080p.WEB-DL-GRP"),
                    "download_url": self.nzb_url(guid),
                    "download_client_id": client_id,
                    "callback_url": "",
                    "metadata": {},
                    "release": release,
                }),
            )
            .await;
        assert_eq!(resp.status, 200, "{}", resp.text());
        resp.json::<Value>()["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap()
    }

    pub async fn in_flight(&self, client_id: Uuid, guid: &str, external_id: &str) -> Uuid {
        downloads_repo::create_download(
            &self.server.pool,
            downloads_repo::DownloadInput {
                source: "test".to_string(),
                protocol: "usenet".to_string(),
                name: format!("{guid}.S01E01.1080p.WEB-DL-GRP"),
                download_url: self.nzb_url(guid),
                download_client_id: client_id,
                external_id: Some(external_id.to_string()),
                callback_url: Some(String::new()),
                metadata: Some(json!({})),
                status: "downloading".to_string(),
                media_kind: Some("tv".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap()
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    pub fn touch(&self, rel: &str) {
        let path = self.path(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
    }

    pub fn set(&self, id: &str, update: impl FnOnce(&mut JobStatus)) {
        let mut jobs = self.engine.jobs.lock().unwrap();
        update(jobs.get_mut(id).expect("engine job"));
    }

    pub fn seed_job(&self, id: &str, state: JobState) {
        self.engine.jobs.lock().unwrap().insert(
            id.to_string(),
            JobStatus {
                id: id.to_string(),
                name: id.to_string(),
                state,
                progress: 0.0,
                total_bytes: 0,
                downloaded_bytes: 0,
                storage: None,
                error: None,
                failure: None,
            },
        );
    }

    pub fn answer_polls_with(&self, status: Option<StatusCode>) {
        *self.engine.poll_status.lock().unwrap() = status;
    }

    pub fn added(&self) -> Vec<JobRequest> {
        self.engine.added.lock().unwrap().clone()
    }

    pub fn deleted(&self) -> Vec<(String, bool)> {
        self.engine.deleted.lock().unwrap().clone()
    }

    pub fn unauthorized(&self) -> usize {
        *self.engine.unauthorized.lock().unwrap()
    }

    pub async fn poll(&self) {
        kobload_api::polling::poll_once(&self.server.app_state).await;
    }

    pub async fn row(&self, id: Uuid) -> DownloadRow {
        sqlx::query_as::<_, DownloadRow>(
            "SELECT status, external_id, progress, error, last_poll_error, files_json,
                    download_client_id, submission_attempts
             FROM downloads WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&self.server.pool)
        .await
        .unwrap()
    }

    pub async fn cleanup(self) {
        self.server.cleanup().await;
    }
}

#[derive(Debug, sqlx::FromRow)]
pub struct DownloadRow {
    pub status: String,
    pub external_id: Option<String>,
    pub progress: Option<f64>,
    pub error: Option<String>,
    pub last_poll_error: Option<String>,
    pub files_json: Option<Value>,
    pub download_client_id: Option<Uuid>,
    pub submission_attempts: i32,
}
