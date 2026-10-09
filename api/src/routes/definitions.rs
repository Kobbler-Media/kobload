//! Definition listing and management endpoints.

use std::collections::HashMap;
use std::path::Path as StdPath;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Json, Router, routing::get};
use serde::Serialize;
use tracing::{error, info};

use crate::indexers::cardigann::reload_definitions;
use crate::state::AppState;

const PROWLARR_INDEXERS_TARBALL: &str =
    "https://github.com/Prowlarr/Indexers/archive/refs/heads/master.tar.gz";
const TARBALL_PREFIX: &str = "Indexers-master/definitions/v11/";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/admin/definitions", get(list_definitions))
        .route(
            "/api/v1/admin/definitions/update",
            axum::routing::post(update_definitions),
        )
        .route(
            "/api/v1/admin/definitions/{name}/test",
            axum::routing::post(test_definition),
        )
}

#[derive(Serialize)]
struct DefinitionSummary {
    name: String,
    description: String,
    site_type: Option<String>,
    language: Option<String>,
    links: Vec<String>,
    settings: Vec<SettingSummary>,
}

#[derive(Serialize)]
struct SettingSummary {
    name: String,
    setting_type: String,
    label: Option<String>,
    default: Option<String>,
    options: Option<HashMap<String, String>>,
    /// For well-known info types without label/default, a built-in description.
    #[serde(skip_serializing_if = "Option::is_none")]
    info_description: Option<String>,
}

async fn list_definitions(State(state): State<AppState>) -> impl IntoResponse {
    let mut defs: Vec<DefinitionSummary> = state
        .definitions
        .iter()
        .map(|entry| {
            let def = entry.value();
            DefinitionSummary {
                name: entry.key().clone(),
                description: def.description.clone(),
                site_type: def.site_type.clone(),
                language: def.language.clone(),
                links: def.links.clone(),
                settings: def
                    .settings
                    .iter()
                    .map(|s| SettingSummary {
                        name: s.name.clone(),
                        setting_type: s.setting_type.clone(),
                        label: s.label.clone(),
                        default: s.default.as_ref().map(|d| d.0.clone()),
                        options: s.options.clone(),
                        info_description: s.info_description().map(String::from),
                    })
                    .collect(),
            }
        })
        .collect();
    defs.sort_by(|a, b| a.name.cmp(&b.name));
    Json(defs)
}

#[derive(serde::Deserialize)]
struct TestRequest {
    #[serde(default)]
    config: std::collections::HashMap<String, String>,
}

async fn test_definition(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<TestRequest>,
) -> impl IntoResponse {
    let def = match state.definitions.get(&name) {
        Some(d) => d.value().clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "definition not found"})),
            );
        }
    };

    let engine = cardigann::CardigannEngine::new(state.http_client.clone());
    match engine.test_login(&def, &body.config).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// Download the latest Prowlarr/Indexers definitions from GitHub and reload the cache.
async fn update_definitions(State(state): State<AppState>) -> impl IntoResponse {
    let definitions_dir = std::env::var("CARDIGANN_DEFINITIONS_DIR")
        .unwrap_or_else(|_| "/data/definitions".to_string());
    let override_dir = std::env::var("CARDIGANN_DEFINITIONS_OVERRIDE_DIR").ok();

    // Download and extract in a blocking task
    let dir = definitions_dir.clone();
    let http = state.http_client.clone();
    let result = tokio::task::spawn_blocking(move || download_definitions(&dir, &http)).await;

    match result {
        Ok(Ok(count)) => {
            reload_definitions(
                &state.definitions,
                StdPath::new(&definitions_dir),
                override_dir.as_deref().map(StdPath::new),
            );
            let total = state.definitions.len();
            info!(
                downloaded = count,
                total, "Updated cardigann definitions from GitHub"
            );
            (
                StatusCode::OK,
                Json(serde_json::json!({"downloaded": count, "total": total})),
            )
        }
        Ok(Err(e)) => {
            error!(error = %e, "Failed to update definitions");
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": e})),
            )
        }
        Err(e) => {
            error!(error = %e, "Definition update task panicked");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "internal error"})),
            )
        }
    }
}

/// Download the Prowlarr/Indexers tarball and extract YAML files to `dest_dir`.
/// Must be called from a blocking context (e.g. `spawn_blocking`).
pub fn download_definitions(dest_dir: &str, http: &reqwest::Client) -> Result<usize, String> {
    // We're in spawn_blocking, so use a temporary runtime for the async download
    let bytes = std::thread::scope(|_| {
        tokio::runtime::Handle::current().block_on(async {
            http.get(PROWLARR_INDEXERS_TARBALL)
                .send()
                .await
                .map_err(|e| format!("download failed: {e}"))?
                .bytes()
                .await
                .map_err(|e| format!("read failed: {e}"))
        })
    })?;

    let decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut archive = tar::Archive::new(decoder);

    std::fs::create_dir_all(dest_dir).map_err(|e| format!("mkdir failed: {e}"))?;

    let mut count = 0;
    for entry in archive.entries().map_err(|e| format!("tar error: {e}"))? {
        let mut entry = entry.map_err(|e| format!("entry error: {e}"))?;
        let path = entry.path().map_err(|e| format!("path error: {e}"))?;
        let path_str = path.to_string_lossy();

        if !path_str.starts_with(TARBALL_PREFIX) {
            continue;
        }
        let filename = &path_str[TARBALL_PREFIX.len()..];
        if filename.is_empty() || !filename.ends_with(".yml") {
            continue;
        }

        let dest = std::path::PathBuf::from(dest_dir).join(filename);
        // Flatten nested dirs — only take the filename
        if let Some(name) = dest.file_name() {
            let flat_dest = std::path::PathBuf::from(dest_dir).join(name);
            let mut file =
                std::fs::File::create(&flat_dest).map_err(|e| format!("write failed: {e}"))?;
            std::io::copy(&mut entry, &mut file).map_err(|e| format!("copy failed: {e}"))?;
            count += 1;
        }
    }

    Ok(count)
}
