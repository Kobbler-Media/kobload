use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use tracing::{info, warn};
use usenet_proto::{JobFailure, JobState, JobStatus};

use crate::helpers::{broadcast_download_updated, log_event};
use crate::repositories::clients as clients_repo;
use crate::repositories::downloads::{self as downloads_repo, InFlightDownload, Requeue};
use crate::state::AppState;

static REPORTED_ORPHANS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

pub async fn reconcile_jobs(state: &AppState) {
    let Some(engine) = state.debrid_providers.usenet_engine() else {
        return;
    };
    let has_client = clients_repo::list_clients(&state.pool)
        .await
        .is_ok_and(|cs| cs.iter().any(|c| c.client_type == "usenet"));
    if !has_client {
        return;
    }
    // Rows are read before the engine's list: a row that has its job id was added to the
    // engine before it was read, so it is in the list unless the engine lost it.
    let rows = match downloads_repo::in_flight_for_client_type(&state.pool, "usenet").await {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "Could not load usenet downloads to reconcile");
            return;
        }
    };
    let jobs = match engine.jobs().await {
        Ok(jobs) => jobs,
        Err(e) => {
            warn!(error = %e, "Could not list usenet engine jobs to reconcile");
            return;
        }
    };

    for row in &rows {
        match jobs.iter().find(|j| j.id == row.external_id) {
            None => requeue(state, row, Requeue::Lost, "the usenet engine lost the job").await,
            Some(job)
                if job.state == JobState::Failed && job.failure != Some(JobFailure::Content) =>
            {
                if let Err(e) = engine.delete_job(&job.id, true).await {
                    warn!(job = %job.id, error = %e, "Could not release a failed usenet engine job");
                    continue;
                }
                let reason = job.error.as_deref().unwrap_or("usenet engine failure");
                requeue(state, row, Requeue::Retry, reason).await;
            }
            Some(_) => {}
        }
    }

    report_orphans(state, &jobs).await;
}

async fn requeue(state: &AppState, row: &InFlightDownload, kind: Requeue, reason: &str) {
    let status = match downloads_repo::requeue_submission(
        &state.pool,
        row.id,
        &row.external_id,
        kind,
        reason,
    )
    .await
    {
        Ok(Some(status)) => status,
        Ok(None) => return,
        Err(e) => {
            warn!(download_id = %row.id, error = %e, "Could not requeue a usenet download");
            return;
        }
    };
    info!(download_id = %row.id, job = %row.external_id, status, reason, "Requeued usenet download");
    log_event(
        &state.pool,
        row.id,
        if status == "failed" {
            "submission_retry_exhausted"
        } else {
            "requeued"
        },
        &format!("Usenet engine job {} ended: {reason}", row.external_id),
        Some(serde_json::json!({ "external_id": row.external_id, "kind": format!("{kind:?}") })),
    )
    .await;
    broadcast_download_updated(&state.pool, &state.download_events, row.id).await;
}

async fn report_orphans(state: &AppState, jobs: &[JobStatus]) {
    let ids: Vec<String> = jobs.iter().map(|j| j.id.clone()).collect();
    let Ok(known) = downloads_repo::known_external_ids(&state.pool, &ids).await else {
        return;
    };
    let known: HashSet<&str> = known.iter().map(String::as_str).collect();
    let mut reported = REPORTED_ORPHANS.lock().unwrap_or_else(|e| e.into_inner());
    for job in jobs.iter().filter(|j| !known.contains(j.id.as_str())) {
        if reported.insert(job.id.clone()) {
            warn!(job = %job.id, name = %job.name, state = ?job.state, "Usenet engine job has no kobload download, leaving it alone");
        }
    }
}
