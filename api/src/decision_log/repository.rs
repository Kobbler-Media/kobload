//! Database operations for decision log tables.

use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use super::types::{
    FlushPayload, ProviderCallDetail, ReleaseDetail, SessionDetail, SessionExport, SessionListItem,
};

/// Flush a complete session (with provider calls and releases) in one transaction.
pub async fn flush_session(pool: &PgPool, payload: FlushPayload) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;

    sqlx::query!(
        r#"
        INSERT INTO decision_log_sessions (
            id, trigger_type, watch_pattern_id, indexer_source_id, is_test,
            outcome, download_id, releases_evaluated, search_queries,
            error_message, started_at, finished_at, duration_ms, created_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, NOW())
        "#,
        payload.session_id,
        payload.trigger_type,
        payload.watch_pattern_id,
        payload.indexer_source_id,
        payload.is_test,
        payload.outcome,
        payload.download_id,
        payload.releases_evaluated,
        payload.search_queries,
        payload.error_message,
        payload.started_at,
        payload.finished_at,
        payload.duration_ms,
    )
    .execute(&mut *tx)
    .await?;

    for call in &payload.provider_calls {
        sqlx::query!(
            r#"
            INSERT INTO decision_log_provider_calls (
                id, session_id, indexer_source_id, source_name, handler_type,
                search_type, query, categories, external_ids,
                results_count, results_after_dedup, status, error_message,
                duration_ms, created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, NOW())
            "#,
            call.id,
            payload.session_id,
            call.indexer_source_id,
            call.source_name,
            call.handler_type,
            call.search_type,
            call.query,
            call.categories,
            call.external_ids,
            call.results_count,
            call.results_after_dedup,
            call.status,
            call.error_message,
            call.duration_ms,
        )
        .execute(&mut *tx)
        .await?;
    }

    for release in &payload.releases {
        let breakdown_json = release
            .quality_score_breakdown
            .as_ref()
            .and_then(|b| serde_json::to_value(b).ok());
        let structural_json = release
            .structural_result
            .as_ref()
            .and_then(|s| serde_json::to_value(s).ok());

        sqlx::query!(
            r#"
            INSERT INTO decision_log_releases (
                id, session_id, provider_call_id, release_title, release_guid,
                release_size, protocol, indexer_name,
                match_score, matched_pattern, match_mode,
                quality_score, quality_score_breakdown, total_score,
                parsed_quality, structural_result,
                decision, decision_reason, episode_key, is_best,
                release_data, created_at
            ) VALUES (
                gen_random_uuid(), $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, NOW()
            )
            "#,
            payload.session_id,
            release.provider_call_id,
            release.release_title,
            release.release_guid,
            release.release_size,
            release.protocol,
            release.indexer_name,
            release.match_score,
            release.matched_pattern,
            release.match_mode,
            release.quality_score,
            breakdown_json,
            release.total_score,
            release.parsed_quality,
            structural_json,
            release.decision,
            release.decision_reason,
            release.episode_key,
            release.is_best,
            release.release_data,
        )
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

/// List sessions with pagination and filtering.
#[allow(clippy::too_many_arguments)]
pub async fn list_sessions(
    pool: &PgPool,
    trigger_types: Option<&[&str]>,
    is_test: Option<bool>,
    watch_pattern_id: Option<Uuid>,
    limit: i64,
    offset: i64,
) -> Result<Vec<SessionListItem>, sqlx::Error> {
    // Use runtime query for dynamic filtering.
    let rows = sqlx::query_as!(
        SessionListItemRow,
        r#"
        SELECT
            s.id,
            s.trigger_type,
            s.watch_pattern_id,
            s.indexer_source_id,
            s.is_test,
            s.outcome,
            s.download_id,
            s.releases_evaluated,
            s.search_queries,
            s.error_message,
            s.started_at,
            s.finished_at,
            s.duration_ms,
            s.created_at,
            COALESCE(
                (SELECT array_to_string(patterns, ', ') FROM watch_patterns WHERE id = s.watch_pattern_id),
                NULL
            ) AS "pattern_name?",
            COALESCE(
                (SELECT name FROM indexer_sources WHERE id = s.indexer_source_id),
                NULL
            ) AS "source_name?",
            (SELECT metadata FROM watch_patterns WHERE id = s.watch_pattern_id) AS "pattern_metadata?"
        FROM decision_log_sessions s
        WHERE
            ($1::text[] IS NULL OR s.trigger_type = ANY($1))
            AND ($2::bool IS NULL OR s.is_test = $2)
            AND ($3::uuid IS NULL OR s.watch_pattern_id = $3)
        ORDER BY s.created_at DESC
        LIMIT $4 OFFSET $5
        "#,
        trigger_types.map(|t| t.iter().map(|s| s.to_string()).collect::<Vec<_>>()) as Option<Vec<String>>,
        is_test,
        watch_pattern_id,
        limit,
        offset,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(|r| r.into()).collect())
}

#[derive(Debug)]
struct SessionListItemRow {
    id: Uuid,
    trigger_type: String,
    watch_pattern_id: Option<Uuid>,
    indexer_source_id: Option<Uuid>,
    is_test: bool,
    outcome: Option<String>,
    download_id: Option<Uuid>,
    releases_evaluated: i32,
    search_queries: Option<serde_json::Value>,
    error_message: Option<String>,
    started_at: OffsetDateTime,
    finished_at: Option<OffsetDateTime>,
    duration_ms: Option<i32>,
    created_at: OffsetDateTime,
    pattern_name: Option<String>,
    source_name: Option<String>,
    pattern_metadata: Option<serde_json::Value>,
}

impl From<SessionListItemRow> for SessionListItem {
    fn from(r: SessionListItemRow) -> Self {
        Self {
            id: r.id,
            trigger_type: r.trigger_type,
            watch_pattern_id: r.watch_pattern_id,
            indexer_source_id: r.indexer_source_id,
            is_test: r.is_test,
            outcome: r.outcome,
            download_id: r.download_id,
            releases_evaluated: r.releases_evaluated,
            search_queries: r.search_queries,
            error_message: r.error_message,
            started_at: r.started_at,
            finished_at: r.finished_at,
            duration_ms: r.duration_ms,
            created_at: r.created_at,
            pattern_name: r.pattern_name,
            source_name: r.source_name,
            pattern_metadata: r.pattern_metadata,
        }
    }
}

/// Get full session detail including provider calls and releases.
pub async fn get_session_detail(
    pool: &PgPool,
    session_id: Uuid,
) -> Result<Option<SessionDetail>, sqlx::Error> {
    let session = sqlx::query!(
        r#"
        SELECT
            s.id,
            s.trigger_type,
            s.watch_pattern_id,
            s.indexer_source_id,
            s.is_test,
            s.outcome,
            s.download_id,
            s.releases_evaluated,
            s.search_queries,
            s.error_message,
            s.started_at,
            s.finished_at,
            s.duration_ms,
            s.created_at,
            COALESCE(
                (SELECT array_to_string(patterns, ', ') FROM watch_patterns WHERE id = s.watch_pattern_id),
                NULL
            ) AS "pattern_name?",
            COALESCE(
                (SELECT name FROM indexer_sources WHERE id = s.indexer_source_id),
                NULL
            ) AS "source_name?",
            (SELECT metadata FROM watch_patterns WHERE id = s.watch_pattern_id) AS "pattern_metadata?"
        FROM decision_log_sessions s
        WHERE s.id = $1
        "#,
        session_id,
    )
    .fetch_optional(pool)
    .await?;

    let Some(s) = session else { return Ok(None) };

    let provider_calls = sqlx::query_as!(
        ProviderCallDetail,
        r#"
        SELECT
            id,
            indexer_source_id,
            source_name,
            handler_type,
            search_type,
            query,
            categories,
            external_ids,
            results_count,
            results_after_dedup,
            status,
            error_message,
            duration_ms
        FROM decision_log_provider_calls
        WHERE session_id = $1
        ORDER BY created_at
        "#,
        session_id,
    )
    .fetch_all(pool)
    .await?;

    let releases = sqlx::query_as!(
        ReleaseDetail,
        r#"
        SELECT
            id,
            provider_call_id,
            release_title,
            release_guid,
            release_size,
            protocol,
            indexer_name,
            match_score,
            matched_pattern,
            match_mode,
            quality_score,
            quality_score_breakdown,
            total_score,
            parsed_quality,
            structural_result,
            decision,
            decision_reason,
            episode_key,
            is_best,
            release_data
        FROM decision_log_releases
        WHERE session_id = $1
        ORDER BY is_best DESC, total_score DESC NULLS LAST, created_at
        "#,
        session_id,
    )
    .fetch_all(pool)
    .await?;

    Ok(Some(SessionDetail {
        id: s.id,
        trigger_type: s.trigger_type,
        watch_pattern_id: s.watch_pattern_id,
        indexer_source_id: s.indexer_source_id,
        is_test: s.is_test,
        outcome: s.outcome,
        download_id: s.download_id,
        releases_evaluated: s.releases_evaluated,
        search_queries: s.search_queries,
        error_message: s.error_message,
        started_at: s.started_at,
        finished_at: s.finished_at,
        duration_ms: s.duration_ms,
        created_at: s.created_at,
        pattern_name: s.pattern_name,
        source_name: s.source_name,
        pattern_metadata: s.pattern_metadata,
        provider_calls,
        releases,
    }))
}

/// Get full session as export blob.
pub async fn get_session_export(
    pool: &PgPool,
    session_id: Uuid,
) -> Result<Option<SessionExport>, sqlx::Error> {
    let detail = get_session_detail(pool, session_id).await?;
    Ok(detail.map(|session| SessionExport { session }))
}

/// Delete a single session (cascades to children).
pub async fn delete_session(pool: &PgPool, session_id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "DELETE FROM decision_log_sessions WHERE id = $1",
        session_id,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Bulk delete sessions by trigger types.
pub async fn delete_sessions_bulk(
    pool: &PgPool,
    trigger_types: &[&str],
) -> Result<u64, sqlx::Error> {
    let types: Vec<String> = trigger_types.iter().map(|s| s.to_string()).collect();
    let result = sqlx::query!(
        "DELETE FROM decision_log_sessions WHERE trigger_type = ANY($1)",
        &types,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Delete old sessions for retention.
pub async fn cleanup_old_sessions(
    pool: &PgPool,
    non_test_max_age_days: i32,
    test_max_age_days: i32,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        DELETE FROM decision_log_sessions
        WHERE
            (is_test = FALSE AND created_at < NOW() - make_interval(days => $1))
            OR (is_test = TRUE AND created_at < NOW() - make_interval(days => $2))
        "#,
        non_test_max_age_days,
        test_max_age_days,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}
