use sqlx::PgPool;
use uuid::Uuid;

use super::super::Result;

/// `fetch_pending_retry_downloads` stops resubmitting at this many attempts; its
/// backoff schedule is written out to the same length.
pub const MAX_SUBMISSION_ATTEMPTS: i32 = 10;

#[derive(Debug)]
pub struct InFlightDownload {
    pub id: Uuid,
    pub external_id: String,
}

pub async fn in_flight_for_client_type(
    pool: &PgPool,
    client_type: &str,
) -> Result<Vec<InFlightDownload>> {
    let rows = sqlx::query_as!(
        InFlightDownload,
        r#"
        SELECT d.id, d.external_id AS "external_id!"
        FROM downloads d
        JOIN download_clients c ON c.id = d.download_client_id
        WHERE c.client_type = $1
          AND d.status IN ('queued', 'downloading')
          AND d.external_id IS NOT NULL
        "#,
        client_type,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn known_external_ids(pool: &PgPool, external_ids: &[String]) -> Result<Vec<String>> {
    let ids = sqlx::query_scalar!(
        r#"SELECT DISTINCT external_id AS "external_id!" FROM downloads WHERE external_id = ANY($1)"#,
        external_ids,
    )
    .fetch_all(pool)
    .await?;
    Ok(ids)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requeue {
    /// The client lost the job: resubmit straight away, without spending an attempt.
    Lost,
    /// The client failed the job for reasons outside the release: back off, spend an
    /// attempt, and fail (without excluding the release) once attempts run out.
    Retry,
}

/// `None` when the download moved on (finished, cancelled, resubmitted) since it was read.
pub async fn requeue_submission(
    pool: &PgPool,
    id: Uuid,
    external_id: &str,
    kind: Requeue,
    reason: &str,
) -> Result<Option<String>> {
    let retry = kind == Requeue::Retry;
    let status = sqlx::query_scalar!(
        r#"
        UPDATE downloads SET
            status = CASE WHEN $3 AND submission_attempts + 1 >= $5 THEN 'failed' ELSE 'pending_retry' END,
            error = CASE WHEN $3 AND submission_attempts + 1 >= $5 THEN $4 ELSE error END,
            external_id = NULL,
            progress = NULL,
            submission_error = $4,
            submission_attempts = submission_attempts + CASE WHEN $3 THEN 1 ELSE 0 END,
            submission_last_attempt_at = CASE WHEN $3 THEN NOW() ELSE NULL END,
            updated_at = NOW()
        WHERE id = $1 AND external_id = $2 AND status IN ('queued', 'downloading')
        RETURNING status
        "#,
        id,
        external_id,
        retry,
        reason,
        MAX_SUBMISSION_ATTEMPTS,
    )
    .fetch_optional(pool)
    .await?;
    Ok(status)
}
