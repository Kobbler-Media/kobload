//! Job system for background tasks via NATS JetStream.
//!
//! All jobs are processed through NATS:
//! - Jobs are published to NATS JetStream for reliable delivery
//! - Job status is tracked in `job_status` table for UI queries
//! - Handlers implement the JobHandler trait for job execution

pub mod handlers;
pub mod worker;

use crate::repositories::jobs as jobs_repo;
use nats_jobs::{JobStatusDb, OptionalPublisher, Publisher};
use serde_json::Value;
use sqlx::PgPool;
use thiserror::Error;
use tracing::warn;
use uuid::Uuid;

// Re-export useful types from nats-jobs
pub use nats_jobs::{JobContext, JobStatus, JobStatusRow};

/// Errors that can occur during job operations.
#[derive(Error, Debug)]
pub enum JobError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Job not found: {0}")]
    NotFound(Uuid),

    #[error("Handler not found for job type: {0}")]
    HandlerNotFound(String),

    #[error("Job execution failed: {0}")]
    ExecutionFailed(String),

    #[error("Retryable error: {0}")]
    Retryable(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("NATS error: {0}")]
    Nats(String),

    #[error("Repository error: {0}")]
    Repository(#[from] crate::repositories::RepositoryError),

    #[error("Job cancelled")]
    Cancelled,
}

impl JobError {
    /// Check if this error is retryable.
    pub fn is_retryable(&self) -> bool {
        match self {
            // Database errors might be transient (connection loss, etc.)
            JobError::Database(e) => {
                let msg = e.to_string().to_lowercase();
                msg.contains("connection") || msg.contains("timeout") || msg.contains("temporarily")
            }
            // Explicitly marked retryable errors
            JobError::Retryable(_) => true,
            // NATS errors might be transient
            JobError::Nats(msg) => {
                let msg = msg.to_lowercase();
                msg.contains("connection") || msg.contains("timeout")
            }
            // Repository errors - check for database errors within
            JobError::Repository(e) => {
                let msg = e.to_string().to_lowercase();
                msg.contains("connection") || msg.contains("timeout") || msg.contains("temporarily")
            }
            // Cancellation is not retryable
            JobError::Cancelled => false,
            // Other errors are permanent
            JobError::NotFound(_)
            | JobError::HandlerNotFound(_)
            | JobError::ExecutionFailed(_)
            | JobError::Json(_) => false,
        }
    }

    /// Create a retryable error.
    pub fn retryable(msg: impl Into<String>) -> Self {
        JobError::Retryable(msg.into())
    }
}

impl From<nats_jobs::JobError> for JobError {
    fn from(e: nats_jobs::JobError) -> Self {
        match e {
            nats_jobs::JobError::Cancelled => JobError::Cancelled,
            _ if e.is_retryable() => JobError::Retryable(e.to_string()),
            _ => JobError::ExecutionFailed(e.to_string()),
        }
    }
}

pub type JobResult<T> = Result<T, JobError>;

/// Enqueue a job for processing via NATS.
///
/// The job is published to NATS JetStream and recorded in the `job_status`
/// table for UI visibility.
pub async fn enqueue(
    pool: &PgPool,
    nats: &Publisher,
    job_type: &str,
    payload: Value,
    context: JobContext,
) -> JobResult<Uuid> {
    let job_id = nats
        .enqueue_job_with_context("kobload", job_type, payload.clone(), context.clone())
        .await
        .map_err(|e| JobError::Nats(e.to_string()))?;

    // Write initial status to DB for UI visibility
    let _ = jobs_repo::insert_job_status(
        pool,
        job_id,
        job_type,
        &payload,
        context.entity_type.as_deref(),
        context.entity_id,
        &context.description,
    )
    .await;

    tracing::info!(job_id = %job_id, job_type = %job_type, description = %context.description, "Job enqueued");
    Ok(job_id)
}

/// Enqueue a job via NATS using an OptionalPublisher.
///
/// Returns `None` if NATS is unavailable or enqueue fails (logged as warning).
/// Returns `Some(job_id)` on success.
pub async fn enqueue_optional(
    pool: &PgPool,
    publisher: &OptionalPublisher,
    job_type: &str,
    payload: Value,
    context: JobContext,
) -> Option<Uuid> {
    let inner = publisher.get()?;
    match enqueue(pool, inner, job_type, payload, context).await {
        Ok(id) => Some(id),
        Err(e) => {
            warn!(job_type = %job_type, error = %e, "Failed to enqueue job via NATS");
            None
        }
    }
}

/// Check if a job of the given type with matching payload is already active.
pub async fn has_active_job(
    pool: &PgPool,
    job_type: &str,
    payload_key: &str,
    payload_value: &str,
) -> JobResult<bool> {
    let db = JobStatusDb::new(pool.clone());
    Ok(db
        .has_active_job(job_type, payload_key, payload_value)
        .await?)
}

/// List recent jobs with pagination.
pub async fn list_jobs(
    pool: &PgPool,
    limit: i64,
    offset: i64,
    status_filter: Option<&str>,
    job_type_filter: Option<&str>,
    exclude_routine: bool,
) -> JobResult<Vec<JobStatusRow>> {
    let db = JobStatusDb::new(pool.clone());
    Ok(db
        .list_recent(
            limit,
            offset,
            status_filter,
            job_type_filter,
            exclude_routine,
            &[],
        )
        .await?)
}

/// Get count of active (pending/running) jobs.
pub async fn running_count(pool: &PgPool, exclude_routine: bool) -> JobResult<i64> {
    let db = JobStatusDb::new(pool.clone());
    let counts = db.job_counts(exclude_routine, &[]).await?;
    Ok(counts.pending + counts.running + counts.retrying)
}

/// Get job counts by status.
pub async fn job_counts(pool: &PgPool, exclude_routine: bool) -> JobResult<nats_jobs::JobCounts> {
    let db = JobStatusDb::new(pool.clone());
    Ok(db.job_counts(exclude_routine, &[]).await?)
}
