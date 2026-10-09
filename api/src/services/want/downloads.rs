use uuid::Uuid;

use crate::helpers::download_url_context_for_indexer_source;
use crate::services::downloads::{DownloadServiceContext, DownloadSubmission, submit_download};
use crate::watch::ProcessedRelease;

use super::types::WantServiceContext;

pub(super) async fn create_download(
    ctx: &WantServiceContext<'_>,
    release: &ProcessedRelease,
    callback_url: &str,
    auth_token: &Option<String>,
    metadata: &Option<serde_json::Value>,
    media_kind: &str,
) -> Result<Uuid, String> {
    let download_url = if release.release.protocol == "torrent" {
        release
            .release
            .magnet_url
            .clone()
            .or_else(|| release.release.download_url.clone())
    } else {
        release.release.download_url.clone()
    }
    .ok_or("Selected release has no download URL")?;

    let metadata = metadata.clone().unwrap_or(serde_json::json!({}));
    let url_context =
        download_url_context_for_indexer_source(ctx.pool, release.release.indexer_source_id)
            .await?;

    let submission = DownloadSubmission {
        source: release.release.indexer.clone(),
        protocol: release.release.protocol.clone(),
        name: release.release.title.clone(),
        download_url,
        download_client_id: None,
        callback_url: Some(callback_url.to_string()),
        auth_token: auth_token.clone(),
        metadata,
        release_payload: Some(serde_json::json!({
            "guid": release.release.guid,
            "title": release.release.title,
            "indexer": release.release.indexer,
            "size": release.release.size,
            "protocol": release.release.protocol,
            "score": release.match_score.unwrap_or(0.0),
            "quality_score": release.quality_score,
            "total_score": release.total_score,
            "matched_pattern": release.matched_pattern,
            "matched_target": release.matched_target,
        })),
        api_key_id: None,
        url_context,
        indexer_source_id: release.release.indexer_source_id,
        quality_profile_id: None,
        // Drives parse_release in metadata_extraction → the interpretation kobbler
        // ingests. Must reflect the want's kind ("sports") so sport
        // location/round/session survive instead of being parsed as a movie.
        media_kind: Some(media_kind.to_string()),
        skip_duplicate_url: true,
    };

    let download_ctx = DownloadServiceContext {
        pool: ctx.pool,
        http_client: ctx.http_client,
        download_events: ctx.download_events,
        download_clients: ctx.download_clients,
        indexers: ctx.indexers,
    };

    let outcome = submit_download(&download_ctx, submission)
        .await
        .map_err(|e| e.to_string())?;

    Ok(outcome.id)
}
