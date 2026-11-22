use std::path::{Component, Path as FsPath};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use kob_usenet::JobSpec;
use serde::Deserialize;
use usenet_proto::{JobFailure, JobRequest, JobState, JobStatus};

use crate::error::ApiError;
use crate::state::AppState;

pub async fn add(
    State(state): State<AppState>,
    Json(request): Json<JobRequest>,
) -> Result<Json<JobStatus>, ApiError> {
    if !single_component(&request.category) {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "invalid_category")
            .with_message("the category must be one folder name"));
    }
    let nzb = STANDARD.decode(&request.nzb_base64).map_err(|e| {
        ApiError::new(StatusCode::BAD_REQUEST, "invalid_base64").with_message(e.to_string())
    })?;
    let dest_dir = state.download_dir.join(&request.category);
    let status = state
        .engine
        .add_job(JobSpec {
            id: request.id,
            name: request.name,
            category: request.category,
            nzb,
            password: request.password,
            dest_dir,
        })
        .await?;
    tracing::info!(
        job = status.id,
        name = status.name,
        category = status.category,
        "job added"
    );
    Ok(Json(wire(status)))
}

pub async fn list(State(state): State<AppState>) -> Json<Vec<JobStatus>> {
    Json(state.engine.jobs().into_iter().map(wire).collect())
}

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<JobStatus>, ApiError> {
    let status = state
        .engine
        .job(&id)
        .ok_or(kob_usenet::Error::UnknownJob(id))?;
    Ok(Json(wire(status)))
}

#[derive(Deserialize)]
pub struct Removal {
    #[serde(default)]
    delete_files: bool,
}

pub async fn remove(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(removal): Query<Removal>,
) -> Result<StatusCode, ApiError> {
    state.engine.remove_job(&id, removal.delete_files).await?;
    tracing::info!(job = id, delete_files = removal.delete_files, "job removed");
    Ok(StatusCode::NO_CONTENT)
}

fn single_component(name: &str) -> bool {
    let mut components = FsPath::new(name).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    ) && !name.contains('/')
}

const fn wire_state(state: kob_usenet::JobState) -> JobState {
    use kob_usenet::JobState as S;
    match state {
        S::Queued => JobState::Queued,
        S::Downloading => JobState::Downloading,
        S::Verifying => JobState::Verifying,
        S::Repairing => JobState::Repairing,
        S::Unpacking => JobState::Unpacking,
        S::Completed => JobState::Completed,
        S::Failed => JobState::Failed,
    }
}

fn wire(status: kob_usenet::JobStatus) -> JobStatus {
    let progress = status.progress();
    let state = wire_state(status.state);
    JobStatus {
        storage: (state == JobState::Completed).then(|| status.path.to_string_lossy().into_owned()),
        id: status.id,
        name: status.name,
        state,
        progress,
        total_bytes: status.bytes_total,
        downloaded_bytes: status.bytes_done,
        error: status.error,
        failure: status.failure.map(|failure| match failure {
            kob_usenet::JobFailure::Content => JobFailure::Content,
            kob_usenet::JobFailure::Infrastructure => JobFailure::Infrastructure,
        }),
    }
}
