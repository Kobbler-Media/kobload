use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use reqwest::StatusCode as HttpStatus;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use uuid::Uuid;

use super::tv_watch::{TvWatch, usenet_release};
use kobload_api::debrid::DefaultDebridProviderResolver;
use kobload_api::repositories::usenet_releases;
use kobload_api::usenet::{EngineSettings, UsenetEngine};
use kobload_api::watch::Release;
use nats_jobs::NzbRef;
use nats_jobs::types::{StreamOption, StreamSourceType};
use usenet_proto::{
    HealthSample, ImportRequest, ImportResponse, OutputInfo, Purpose, StreamClaims,
};

pub const CONTROL_KEY: &str = "control-key-under-test-control-key-under-test";
pub const STREAM_SECRET: &str = "stream-secret-under-test-stream-secret-under-test";
pub const STREAM_BASE: &str = "http://10.9.9.9:7090";
pub const INDEXER_KEY: &str = "indexer-key-under-test";

async fn serve(app: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.ok() });
    url
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HealthCall {
    pub guid: String,
    pub percent: Option<u8>,
    pub output: Option<usize>,
}

#[derive(Default)]
struct EngineState {
    imports: Vec<ImportRequest>,
    by_cache_key: HashMap<String, ImportResponse>,
    by_release_key: HashMap<String, ImportResponse>,
    layouts: HashMap<String, Vec<OutputInfo>>,
    rejected: HashMap<String, (StatusCode, String)>,
    delay: Duration,
    release_delay: Duration,
    guid_of: HashMap<String, String>,
    health: HashMap<(String, Option<usize>), HealthSample>,
    health_calls: Vec<HealthCall>,
    health_delay: Duration,
}

fn posted_guid(nzb: &[u8]) -> String {
    let text = String::from_utf8_lossy(nzb);
    text.split("subject=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap_or_default()
        .to_string()
}

#[derive(Clone)]
pub struct MockEngine {
    pub url: String,
    state: Arc<Mutex<EngineState>>,
}

fn authorized(headers: &HeaderMap) -> bool {
    headers.get("authorization").and_then(|v| v.to_str().ok())
        == Some(format!("Bearer {CONTROL_KEY}").as_str())
}

async fn import(
    State(state): State<Arc<Mutex<EngineState>>>,
    headers: HeaderMap,
    Json(req): Json<ImportRequest>,
) -> axum::response::Response {
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let delay = {
        let mut s = state.lock().unwrap();
        s.imports.push(req.clone());
        s.delay
    };
    tokio::time::sleep(delay).await;
    let mut s = state.lock().unwrap();
    if let Some(found) = s.by_cache_key.get(&req.cache_key) {
        return Json(found.clone()).into_response();
    }
    let Some(nzb) = req.nzb_base64.as_deref() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "not_cached" })),
        )
            .into_response();
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(nzb)
        .unwrap();
    let guid = posted_guid(&bytes);
    if let Some((status, code)) = s.rejected.get(&guid) {
        return (*status, Json(json!({ "error": code }))).into_response();
    }
    let release_key = format!("nzb-{}", const_hex::encode(Sha256::digest(&bytes)));
    let outputs = s.layouts.get(&guid).cloned().unwrap_or_else(|| {
        vec![OutputInfo {
            index: 0,
            path: format!("{guid}.mkv"),
            size: 1_000_000_000,
        }]
    });
    let response = ImportResponse {
        release_key: release_key.clone(),
        outputs,
    };
    s.by_cache_key.insert(req.cache_key, response.clone());
    s.by_release_key
        .insert(release_key.clone(), response.clone());
    s.guid_of.insert(release_key, guid);
    Json(response).into_response()
}

const HEALTHY: HealthSample = HealthSample {
    checked: 20,
    missing: 0,
};

async fn health(
    State(state): State<Arc<Mutex<EngineState>>>,
    headers: HeaderMap,
    Path(key): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> axum::response::Response {
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let percent = query.get("percent").and_then(|p| p.parse().ok());
    let output = query.get("output").and_then(|p| p.parse().ok());
    let delay = {
        let mut s = state.lock().unwrap();
        let guid = s.guid_of.get(&key).cloned().unwrap_or_default();
        s.health_calls.push(HealthCall {
            guid,
            percent,
            output,
        });
        s.health_delay
    };
    tokio::time::sleep(delay).await;
    let s = state.lock().unwrap();
    if !s.by_release_key.contains_key(&key) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "unknown_release" })),
        )
            .into_response();
    }
    let guid = s.guid_of.get(&key).cloned().unwrap_or_default();
    let sample = s
        .health
        .get(&(guid.clone(), output))
        .or_else(|| s.health.get(&(guid, None)))
        .copied()
        .unwrap_or(HEALTHY);
    Json(sample).into_response()
}

async fn release(
    State(state): State<Arc<Mutex<EngineState>>>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> axum::response::Response {
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let delay = state.lock().unwrap().release_delay;
    tokio::time::sleep(delay).await;
    match state.lock().unwrap().by_release_key.get(&key) {
        Some(found) => Json(found.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response(),
    }
}

impl MockEngine {
    pub async fn start() -> Self {
        let state = Arc::new(Mutex::new(EngineState::default()));
        let app = Router::new()
            .route("/v1/import", post(import))
            .route("/v1/releases/{key}", get(release))
            .route("/v1/releases/{key}/health", post(health))
            .route("/healthz", get(|| async { "ok" }))
            .with_state(state.clone());
        Self {
            url: serve(app).await,
            state,
        }
    }

    pub fn layout(&self, guid: &str, files: &[(&str, u64)]) {
        let outputs = files
            .iter()
            .enumerate()
            .map(|(index, (path, size))| OutputInfo {
                index,
                path: path.to_string(),
                size: *size,
            })
            .collect();
        self.state
            .lock()
            .unwrap()
            .layouts
            .insert(guid.to_string(), outputs);
    }

    pub fn reject(&self, guid: &str) {
        self.refuse(guid, StatusCode::UNPROCESSABLE_ENTITY, "missing_articles");
    }

    pub fn refuse(&self, guid: &str, status: StatusCode, code: &str) {
        self.state
            .lock()
            .unwrap()
            .rejected
            .insert(guid.to_string(), (status, code.to_string()));
    }

    pub fn accept(&self, guid: &str) {
        self.state.lock().unwrap().rejected.remove(guid);
    }

    pub fn release_delay(&self, delay: Duration) {
        self.state.lock().unwrap().release_delay = delay;
    }

    /// The engine holds a release kobload's in-memory file list has never seen.
    pub fn hold(&self, release_key: &str, files: &[(&str, u64)]) {
        let outputs = files
            .iter()
            .enumerate()
            .map(|(index, (path, size))| OutputInfo {
                index,
                path: path.to_string(),
                size: *size,
            })
            .collect();
        self.state.lock().unwrap().by_release_key.insert(
            release_key.to_string(),
            ImportResponse {
                release_key: release_key.to_string(),
                outputs,
            },
        );
    }

    pub fn health(&self, guid: &str, output: Option<usize>, sample: HealthSample) {
        self.state
            .lock()
            .unwrap()
            .health
            .insert((guid.to_string(), output), sample);
    }

    /// An engine restart after the release cache dropped the layout and the NZB.
    pub fn evict(&self, release_key: &str) {
        let mut s = self.state.lock().unwrap();
        s.by_release_key.remove(release_key);
        s.by_cache_key.retain(|_, r| r.release_key != release_key);
    }

    pub fn health_delay(&self, delay: Duration) {
        self.state.lock().unwrap().health_delay = delay;
    }

    pub fn health_calls(&self) -> Vec<HealthCall> {
        self.state.lock().unwrap().health_calls.clone()
    }

    pub fn delay(&self, delay: Duration) {
        self.state.lock().unwrap().delay = delay;
    }

    pub fn imports(&self) -> Vec<ImportRequest> {
        self.state.lock().unwrap().imports.clone()
    }

    pub fn imports_with_nzb(&self) -> usize {
        self.imports()
            .iter()
            .filter(|i| i.nzb_base64.is_some())
            .count()
    }

    pub fn settings(&self) -> EngineSettings {
        EngineSettings {
            engine_url: self.url.clone(),
            stream_base_url: STREAM_BASE.to_string(),
            control_key: CONTROL_KEY.to_string(),
            stream_secret: STREAM_SECRET.to_string(),
        }
    }
}

#[derive(Clone, Copy)]
pub enum Served {
    Status(StatusCode),
    Huge,
}

#[derive(Default)]
struct HostState {
    fetches: Vec<String>,
    served: HashMap<String, Served>,
}

#[derive(Clone)]
pub struct NzbHost {
    pub url: String,
    state: Arc<Mutex<HostState>>,
}

async fn nzb(
    State(state): State<Arc<Mutex<HostState>>>,
    Path(guid): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> axum::response::Response {
    if query.get("apikey").map(String::as_str) != Some(INDEXER_KEY) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let served = {
        let mut s = state.lock().unwrap();
        s.fetches.push(guid.clone());
        s.served.get(&guid).copied()
    };
    match served {
        Some(Served::Status(status)) => status.into_response(),
        Some(Served::Huge) => {
            let chunk = axum::body::Bytes::from(vec![b' '; 1 << 20]);
            let body = futures::stream::iter(
                (0..130).map(move |_| Ok::<_, std::io::Error>(chunk.clone())),
            );
            axum::body::Body::from_stream(body).into_response()
        }
        None => format!("<nzb><file subject=\"{guid}\"/></nzb>").into_response(),
    }
}

impl NzbHost {
    pub async fn start() -> Self {
        let state = Arc::new(Mutex::new(HostState::default()));
        let app = Router::new()
            .route("/nzb/{guid}", get(nzb))
            .with_state(state.clone());
        Self {
            url: serve(app).await,
            state,
        }
    }

    pub fn serve(&self, guid: &str, served: Served) {
        self.state
            .lock()
            .unwrap()
            .served
            .insert(guid.to_string(), served);
    }

    pub fn fetches(&self, guid: &str) -> usize {
        self.state
            .lock()
            .unwrap()
            .fetches
            .iter()
            .filter(|g| *g == guid)
            .count()
    }
}

pub struct UsenetRig {
    pub tv: TvWatch,
    pub engine: MockEngine,
    pub nzbs: NzbHost,
    pub provider: Uuid,
}

impl UsenetRig {
    pub async fn start() -> Self {
        let engine = MockEngine::start().await;
        let nzbs = NzbHost::start().await;
        let usenet = Arc::new(UsenetEngine::new(
            engine.settings(),
            super::local_only_http_client(),
        ));
        let tv = TvWatch::start_with_debrid(Arc::new(DefaultDebridProviderResolver::with_usenet(
            usenet,
        )))
        .await;
        let provider = super::stream_fixtures::create_provider(&tv.server.pool, "usenet")
            .await
            .id;
        Self {
            tv,
            engine,
            nzbs,
            provider,
        }
    }

    pub fn nzb(&self, guid: &str, title: &str) -> Release {
        let mut release = usenet_release(guid, title);
        release.download_url = Some(format!("{}/nzb/{guid}?apikey={INDEXER_KEY}", self.nzbs.url));
        release.indexer_source_id = Some(self.tv.source.id);
        release
    }

    pub fn nzb_ref(&self, guid: &str) -> NzbRef {
        usenet_releases::nzb_ref(&self.tv.source.id.to_string(), guid)
    }

    pub fn cache_key(&self, guid: &str) -> String {
        self.nzb_ref(guid).guid
    }

    /// A listing as a search would store it, without the search.
    pub async fn list(&self, guid: &str, title: &str) -> NzbRef {
        let release = self.nzb(guid, title);
        usenet_releases::upsert(
            &self.tv.server.pool,
            &[usenet_releases::Listing {
                indexer_id: self.tv.source.id.to_string(),
                guid: guid.to_string(),
                download_url: release.download_url.unwrap(),
                release_title: release.title,
            }],
        )
        .await
        .unwrap();
        self.nzb_ref(guid)
    }

    pub async fn options(&self, show: &str, episode: &str) -> Vec<StreamOption> {
        self.tv
            .stream_options(&super::tv_watch::episode_want(show, "1", episode, None))
            .await
            .into_iter()
            .filter(|o| o.source_type == StreamSourceType::Usenet)
            .collect()
    }

    pub async fn resolve(&self, body: Value) -> (HttpStatus, Value) {
        let mut body = body;
        body["debrid_provider_id"] = json!(self.provider);
        let resp = self
            .tv
            .server
            .admin_client()
            .post_json("/api/v1/stream/resolve", &body)
            .await;
        let value = serde_json::from_str(&resp.text()).unwrap_or(Value::String(resp.text()));
        (resp.status, value)
    }

    pub async fn resolve_option(&self, option: &StreamOption) -> Value {
        let (status, body) = self
            .resolve(json!({
                "nzb_ref": option.nzb_ref,
                "release_key": option.release_key,
                "file_id": option.files.first().map(|f| f.id),
                "release_title": option.release_title,
            }))
            .await;
        assert_eq!(status, HttpStatus::OK, "{body}");
        body
    }
}

pub fn claims(url: &str) -> StreamClaims {
    let token = url
        .strip_prefix(&format!("{STREAM_BASE}/stream/"))
        .unwrap_or_else(|| panic!("{url} is not an engine stream URL"));
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    usenet_proto::verify(STREAM_SECRET.as_bytes(), token, now).expect("token verifies")
}

pub fn purposes(resolved: &Value) -> (StreamClaims, StreamClaims) {
    let play = claims(resolved["stream_url"].as_str().unwrap());
    let cache = claims(resolved["cache_url"].as_str().unwrap());
    (play, cache)
}

pub fn assert_purpose(claims: &StreamClaims, purpose: Purpose) {
    assert_eq!(claims.purpose, purpose, "{claims:?}");
}
