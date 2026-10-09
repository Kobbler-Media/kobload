use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use tracing::warn;
use uuid::Uuid;

use crate::auth::require_auth;
use crate::repositories::quality_profiles as quality_repo;
use crate::startup::seed_quality_presets;
use crate::state::AppState;

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct QualityProfileCreate {
    pub name: String,
    pub media_kind: String,
    pub min_resolution: Option<String>,
    pub max_resolution: Option<String>,
    #[serde(default)]
    pub preferred_sources: Vec<String>,
    #[serde(default)]
    pub preferred_codecs: Vec<String>,
    #[serde(default)]
    pub preferred_audio: Vec<String>,
    #[serde(default)]
    pub preferred_languages: Vec<String>,
    #[serde(default)]
    pub rejected_sources: Vec<String>,
    #[serde(default)]
    pub rejected_languages: Vec<String>,
    #[serde(default)]
    pub rejected_terms: Vec<String>,
    #[serde(default)]
    pub reject_hardcoded_subs: bool,
    #[serde(default = "default_true")]
    pub use_trash_filters: bool,
    #[serde(default)]
    pub reject_remux: bool,
    #[serde(default)]
    pub is_default: bool,
    pub upgrade_cutoff_score: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct QualityProfileUpdate {
    pub name: Option<String>,
    pub media_kind: Option<String>,
    pub min_resolution: Option<Option<String>>,
    pub max_resolution: Option<Option<String>>,
    pub preferred_sources: Option<Vec<String>>,
    pub preferred_codecs: Option<Vec<String>>,
    pub preferred_audio: Option<Vec<String>>,
    pub preferred_languages: Option<Vec<String>>,
    pub rejected_sources: Option<Vec<String>>,
    pub rejected_languages: Option<Vec<String>>,
    pub rejected_terms: Option<Vec<String>>,
    pub reject_hardcoded_subs: Option<bool>,
    pub reject_remux: Option<bool>,
    pub use_trash_filters: Option<bool>,
    pub is_default: Option<bool>,
    pub upgrade_cutoff_score: Option<Option<f64>>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/admin/quality-profiles",
            axum::routing::get(list_quality_profiles).post(create_quality_profile),
        )
        .route(
            "/api/v1/admin/quality-profiles/{id}",
            axum::routing::patch(update_quality_profile).delete(delete_quality_profile),
        )
        .route(
            "/api/v1/admin/quality-profiles/reset",
            axum::routing::post(reset_quality_profiles),
        )
        .route(
            "/api/v1/quality-profiles",
            axum::routing::get(list_quality_profiles_public),
        )
}

async fn list_quality_profiles(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let profiles = match quality_repo::list_quality_profiles(&state.pool).await {
        Ok(rows) => rows,
        Err(err) => {
            warn!(error = %err, "Failed to list quality profiles");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
    };

    Json(profiles).into_response()
}

async fn list_quality_profiles_public(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    list_quality_profiles(State(state), headers).await
}

async fn create_quality_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<QualityProfileCreate>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let is_default = payload.is_default;
    let media_kind = payload.media_kind.clone();

    // Clear existing defaults before creating, to avoid unique constraint violation
    if is_default {
        // Use a sentinel UUID that won't match anything - we want to clear ALL defaults
        let _ = quality_repo::clear_defaults_for_media_kind_except(
            &state.pool,
            &media_kind,
            Uuid::nil(),
        )
        .await;
    }

    let input = quality_repo::QualityProfileInput {
        name: payload.name,
        media_kind: payload.media_kind,
        min_resolution: payload.min_resolution,
        max_resolution: payload.max_resolution,
        preferred_sources: payload.preferred_sources,
        preferred_codecs: payload.preferred_codecs,
        preferred_audio: payload.preferred_audio,
        preferred_languages: payload.preferred_languages,
        rejected_sources: payload.rejected_sources,
        rejected_languages: payload.rejected_languages,
        rejected_terms: payload.rejected_terms,
        reject_hardcoded_subs: payload.reject_hardcoded_subs,
        reject_remux: payload.reject_remux,
        use_trash_filters: payload.use_trash_filters,
        is_default,
        upgrade_cutoff_score: payload.upgrade_cutoff_score,
    };

    match quality_repo::create_quality_profile(&state.pool, input).await {
        Ok(id) => Json(serde_json::json!({ "id": id })).into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to create quality profile");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

async fn update_quality_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(payload): Json<QualityProfileUpdate>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let is_default = payload.is_default;
    let media_kind_for_clear = payload.media_kind.clone();

    // Clear existing defaults before updating, to avoid unique constraint violation
    if is_default == Some(true) {
        let media_kind = if let Some(ref mk) = media_kind_for_clear {
            mk.clone()
        } else if let Ok(Some(profile)) =
            quality_repo::get_quality_profile_by_id(&state.pool, id).await
        {
            profile.media_kind
        } else {
            String::new()
        };
        if !media_kind.is_empty() {
            let _ =
                quality_repo::clear_defaults_for_media_kind_except(&state.pool, &media_kind, id)
                    .await;
        }
    }

    let input = quality_repo::QualityProfileUpdate {
        name: payload.name,
        media_kind: payload.media_kind,
        min_resolution: payload.min_resolution,
        max_resolution: payload.max_resolution,
        preferred_sources: payload.preferred_sources,
        preferred_codecs: payload.preferred_codecs,
        preferred_audio: payload.preferred_audio,
        preferred_languages: payload.preferred_languages,
        rejected_sources: payload.rejected_sources,
        rejected_languages: payload.rejected_languages,
        rejected_terms: payload.rejected_terms,
        reject_hardcoded_subs: payload.reject_hardcoded_subs,
        reject_remux: payload.reject_remux,
        use_trash_filters: payload.use_trash_filters,
        is_default,
        upgrade_cutoff_score: payload.upgrade_cutoff_score,
    };

    match quality_repo::update_quality_profile(&state.pool, id, input).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to update quality profile");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

async fn reset_quality_profiles(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    seed_quality_presets(&state.pool).await;

    match quality_repo::list_quality_profiles(&state.pool).await {
        Ok(profiles) => Json(profiles).into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to list quality profiles after reset");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

async fn delete_quality_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match quality_repo::delete_quality_profile(&state.pool, id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to delete quality profile");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}
