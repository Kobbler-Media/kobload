use tracing::info;
use uuid::Uuid;

use crate::helpers::{download_url_context_for_indexer_source, log_event};
use crate::services::downloads::{DownloadServiceContext, DownloadSubmission, submit_download};
use crate::state::AppState;
use crate::watch::ProcessedRelease;

/// Create a download for a scored result
pub(super) async fn create_download(
    state: &AppState,
    release: &ProcessedRelease,
    callback_url: &str,
    auth_token: &Option<String>,
    metadata: &Option<serde_json::Value>,
    media_kind: &str,
) -> Result<Uuid, String> {
    let r = &release.release;

    // Get download URL (prefer magnet for torrents, download_url for usenet)
    let download_url = if r.protocol == "torrent" {
        r.magnet_url.clone().or_else(|| r.download_url.clone())
    } else {
        r.download_url.clone()
    }
    .ok_or("Selected release has no download URL")?;

    let metadata = metadata.clone().unwrap_or(serde_json::json!({}));
    let url_context =
        download_url_context_for_indexer_source(&state.pool, r.indexer_source_id).await?;

    let match_score = release.match_score.unwrap_or(0.0);

    let submission = DownloadSubmission {
        source: r.indexer.clone(),
        protocol: r.protocol.clone(),
        name: r.title.clone(),
        download_url: download_url.clone(),
        download_client_id: None,
        callback_url: Some(callback_url.to_string()),
        auth_token: auth_token.clone(),
        metadata,
        release_payload: Some(serde_json::json!({
            "guid": r.guid,
            "title": r.title,
            "indexer": r.indexer,
            "size": r.size,
            "score": match_score,
            "quality_score": release.quality_score,
            "total_score": release.total_score,
            "matched_pattern": release.matched_pattern,
            "matched_target": release.matched_target,
        })),
        api_key_id: None,
        url_context,
        indexer_source_id: r.indexer_source_id,
        quality_profile_id: None,
        // Drives parse_release for the interpretation kobbler ingests; sport
        // wants must yield "sports" so location/round/session survive.
        media_kind: Some(media_kind.to_string()),
        skip_duplicate_url: false,
    };

    let ctx = DownloadServiceContext {
        pool: &state.pool,
        http_client: &state.http_client,
        download_events: &state.download_events,
        download_clients: state.download_clients.as_ref(),
        indexers: state.indexers.as_ref(),
    };
    let outcome = submit_download(&ctx, submission)
        .await
        .map_err(|e| e.to_string())?;
    let id = outcome.id;

    info!(
        download_id = %id,
        title = %r.title,
        score = match_score,
        matched_target = ?release.matched_target,
        "Created download from search-and-download"
    );

    log_event(
        &state.pool,
        id,
        "search_match",
        &format!("Download created from search (score: {:.2})", match_score),
        Some(serde_json::json!({
            "title": r.title,
            "score": match_score,
            "matched_pattern": release.matched_pattern,
            "matched_target": release.matched_target,
            "indexer": r.indexer,
        })),
    )
    .await;

    Ok(id)
}
