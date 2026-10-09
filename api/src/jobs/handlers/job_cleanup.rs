//! Job cleanup handler — resets orphaned running jobs, reconciles ghost jobs,
//! and deletes old records.

use crate::jobs::worker::JobHandler;
use crate::jobs::{JobError, JobResult};
use async_trait::async_trait;
use nats_jobs::NatsClient;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::HashSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

pub struct JobCleanupHandler {
    pool: PgPool,
    nats: NatsClient,
}

impl JobCleanupHandler {
    pub fn new(pool: PgPool, nats: NatsClient) -> Self {
        Self { pool, nats }
    }
}

#[async_trait]
impl JobHandler for JobCleanupHandler {
    fn job_type(&self) -> &'static str {
        "job_cleanup"
    }

    fn max_concurrent(&self) -> usize {
        1
    }

    async fn execute(
        &self,
        job_id: Uuid,
        _payload: Value,
        cancel: CancellationToken,
    ) -> JobResult<Value> {
        nats_jobs::check_cancelled(&cancel)?;

        info!(job_id = %job_id, "Running job cleanup");

        // Reset orphaned running jobs that haven't been updated in 30 minutes.
        let job_db = nats_jobs::JobStatusDb::new(self.pool.clone());
        let orphaned = job_db.reset_orphaned_running_jobs(30).await.map_err(|e| {
            JobError::ExecutionFailed(format!("Failed to reset orphaned running jobs: {e}"))
        })?;
        if orphaned > 0 {
            info!(orphaned = orphaned, "Reset orphaned running jobs to failed");
        }

        // Ghost job reconciliation: find job_status rows that claim to be active
        // but have no corresponding NATS message.
        let ghosts_cleaned = reconcile_ghost_jobs(&self.pool, &self.nats).await;

        // Mark stale pending jobs as failed (older than 24h with no active processing).
        let result = sqlx::query!(
            r#"
            UPDATE job_status
            SET status = 'failed',
                error = 'Job timed out - marked as stale (no active processing for this job type)',
                completed_at = NOW()
            WHERE status = 'pending'
              AND created_at < NOW() - INTERVAL '24 hours'
              AND job_type NOT IN (
                SELECT DISTINCT job_type FROM job_status
                WHERE status = 'running'
                   OR (status = 'completed' AND completed_at > NOW() - INTERVAL '24 hours')
                   OR (status = 'pending' AND created_at > NOW() - INTERVAL '4 hours')
              )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| JobError::ExecutionFailed(format!("Failed to cleanup jobs: {e}")))?;

        let cleaned = result.rows_affected();
        if cleaned > 0 {
            info!(cleaned = cleaned, "Marked stale pending jobs as failed");
        }

        // Clean up old completed/failed jobs (older than 7 days)
        let deleted = sqlx::query!(
            r#"
            DELETE FROM job_status
            WHERE status IN ('completed', 'failed')
              AND completed_at < NOW() - INTERVAL '7 days'
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| JobError::ExecutionFailed(format!("Failed to delete old jobs: {e}")))?
        .rows_affected();

        if deleted > 0 {
            info!(deleted = deleted, "Deleted old completed/failed jobs");
        }

        Ok(json!({
            "stale_jobs_cleaned": cleaned,
            "old_jobs_deleted": deleted,
            "ghost_jobs_cleaned": ghosts_cleaned,
        }))
    }
}

/// Per-message ghost reconciliation against NATS stream state.
///
/// For each job type with active DB rows (>10 min old), walks the stream
/// messages for that subject to build a set of job IDs NATS actually knows
/// about. Any DB row whose ID is NOT in that set is a confirmed ghost.
async fn reconcile_ghost_jobs(pool: &PgPool, nats: &NatsClient) -> u64 {
    let job_types: Vec<String> = match sqlx::query_scalar(
        r#"
        SELECT DISTINCT job_type
        FROM job_status
        WHERE status IN ('pending', 'running', 'retrying')
          AND created_at < NOW() - INTERVAL '10 minutes'
        "#,
    )
    .fetch_all(pool)
    .await
    {
        Ok(types) => types,
        Err(e) => {
            warn!(error = %e, "Failed to query active job types for ghost reconciliation");
            return 0;
        }
    };

    if job_types.is_empty() {
        return 0;
    }

    let stream = match nats.jetstream().get_stream("JOBS").await {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "Failed to get JOBS stream for ghost reconciliation");
            return 0;
        }
    };

    let mut total_cleaned = 0u64;

    for job_type in &job_types {
        let db_rows: Vec<Uuid> = match sqlx::query_scalar(
            r#"
            SELECT id FROM job_status
            WHERE job_type = $1
              AND status IN ('pending', 'running', 'retrying')
              AND created_at < NOW() - INTERVAL '10 minutes'
            "#,
        )
        .bind(job_type)
        .fetch_all(pool)
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, job_type = %job_type, "Failed to query active jobs");
                continue;
            }
        };

        if db_rows.is_empty() {
            continue;
        }

        let subject = format!("jobs.kobload.{job_type}");
        let nats_job_ids = match collect_stream_job_ids(&stream, &subject).await {
            Ok(ids) => ids,
            Err(e) => {
                warn!(
                    job_type = %job_type,
                    error = %e,
                    "Failed to walk stream messages, skipping ghost check"
                );
                continue;
            }
        };

        let ghost_ids: Vec<Uuid> = db_rows
            .into_iter()
            .filter(|id| !nats_job_ids.contains(id))
            .collect();

        if ghost_ids.is_empty() {
            continue;
        }

        let result: u64 = match sqlx::query(
            r#"
            UPDATE job_status
            SET status = 'failed',
                error = 'Ghost job — no matching NATS message',
                completed_at = NOW()
            WHERE id = ANY($1)
              AND status IN ('pending', 'running', 'retrying')
            "#,
        )
        .bind(&ghost_ids)
        .execute(pool)
        .await
        {
            Ok(r) => r.rows_affected(),
            Err(e) => {
                warn!(error = %e, job_type = %job_type, "Failed to clean ghost jobs");
                continue;
            }
        };

        if result > 0 {
            info!(
                job_type = %job_type,
                count = result,
                "Cleaned ghost jobs (DB rows with no matching NATS message)"
            );
            total_cleaned += result;
        }
    }

    total_cleaned
}

/// Walk stream messages for a subject and collect job IDs from the
/// `Nats-Msg-Id` header (falling back to payload deserialization).
async fn collect_stream_job_ids(
    stream: &async_nats::jetstream::stream::Stream,
    subject: &str,
) -> Result<HashSet<Uuid>, String> {
    use async_nats::jetstream::stream::DirectGetErrorKind;

    let mut ids = HashSet::new();
    let mut seq = 1u64;

    loop {
        match stream.direct_get_next_for_subject(subject, Some(seq)).await {
            Ok(msg) => {
                let job_id =
                    if let Some(id_val) = msg.headers.get(async_nats::header::NATS_MESSAGE_ID) {
                        id_val.as_str().parse::<Uuid>().ok()
                    } else {
                        serde_json::from_slice::<serde_json::Value>(&msg.payload)
                            .ok()
                            .and_then(|v| v.get("id")?.as_str()?.parse::<Uuid>().ok())
                    };

                if let Some(id) = job_id {
                    ids.insert(id);
                } else {
                    debug!(
                        seq = msg.sequence,
                        "Could not extract job ID from stream message"
                    );
                }

                seq = msg.sequence + 1;
            }
            Err(e) if matches!(e.kind(), DirectGetErrorKind::NotFound) => {
                break;
            }
            Err(e) => {
                return Err(format!("direct_get_next_for_subject failed: {e}"));
            }
        }
    }

    Ok(ids)
}
