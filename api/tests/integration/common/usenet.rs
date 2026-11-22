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
use kobload_api::usenet::{EngineSettings, UsenetEngine};
use kobload_api::watch::Release;
use nats_jobs::types::{StreamOption, StreamSourceType};
use usenet_proto::{ImportRequest, ImportResponse, OutputInfo, Purpose, StreamClaims};

pub const CONTROL_KEY: &str = "control-key-under-test";
pub const STREAM_SECRET: &str = "stream-secret-under-test";
pub const STREAM_BASE: &str = "http://10.9.9.9:7090";
pub const INDEXER_KEY: &str = "indexer-key-under-test";

async fn serve(app: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.ok() });
    url
}

#[derive(Default)]
struct EngineState {
    imports: Vec<ImportRequest>,
    by_cache_key: HashMap<String, ImportResponse>,
    by_release_key: HashMap<String, ImportResponse>,
    layouts: HashMap<String, Vec<OutputInfo>>,
    rejected: Vec<String>,
    delay: Duration,
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
    let guid = req
        .cache_key
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string();
    if s.rejected.contains(&guid) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "missing_articles" })),
        )
            .into_response();
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(nzb)
        .unwrap();
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
    s.by_release_key.insert(release_key, response.clone());
    Json(response).into_response()
}

async fn release(
    State(state): State<Arc<Mutex<EngineState>>>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> axum::response::Response {
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
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
        self.state.lock().unwrap().rejected.push(guid.to_string());
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

#[derive(Clone)]
pub struct NzbHost {
    pub url: String,
    fetches: Arc<Mutex<Vec<String>>>,
}

async fn nzb(
    State(fetches): State<Arc<Mutex<Vec<String>>>>,
    Path(guid): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> axum::response::Response {
    if query.get("apikey").map(String::as_str) != Some(INDEXER_KEY) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    fetches.lock().unwrap().push(guid.clone());
    format!("<nzb><file subject=\"{guid}\"/></nzb>").into_response()
}

impl NzbHost {
    pub async fn start() -> Self {
        let fetches = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/nzb/{guid}", get(nzb))
            .with_state(fetches.clone());
        Self {
            url: serve(app).await,
            fetches,
        }
    }

    pub fn fetches(&self, guid: &str) -> usize {
        self.fetches
            .lock()
            .unwrap()
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

    pub fn cache_key(&self, guid: &str) -> String {
        format!("{}/{guid}", self.tv.source.id)
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
