use axum::{
    Json,
    body::Body,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use futures::TryStreamExt;
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};
use tracing::warn;
use uuid::Uuid;

use crate::archive;
use crate::auth::require_auth;
use crate::auth::require_auth_with_token;
use crate::routes::file_serving::serve_plain_file;
use crate::repositories::downloads as downloads_repo;
use crate::state::AppState;
use crate::types::DownloadStreamEvent;

use super::StreamQuery;

pub async fn get_download_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match downloads_repo::list_download_events(&state.pool, id).await {
        Ok(events) => Json(events).into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to fetch download events");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

pub async fn download_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    request: Request,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let file_path = match downloads_repo::get_file_path(&state.pool, id).await {
        Ok(Some(Some(path))) => path,
        Ok(Some(None)) => return (StatusCode::NOT_FOUND, "file not set").into_response(),
        Ok(None) => return (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to fetch download file path");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
    };

    match serve_plain_file(&file_path, request).await {
        Some(response) => response,
        None => (StatusCode::NOT_FOUND, "file not found").into_response(),
    }
}

pub async fn download_file_by_id(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, file_id)): Path<(Uuid, Uuid)>,
    request: Request,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Look up file by ID in files_json
    let file_info = match downloads_repo::get_file_by_id(&state.pool, id, file_id).await {
        Ok(Some(info)) => info,
        Ok(None) => return (StatusCode::NOT_FOUND, "file not found").into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to look up file by ID");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
    };

    let absolute_path = &file_info.absolute_path;

    // Try the file directly first — if it exists on disk, serve it regardless of
    // what the path string looks like. This is ground truth: filenames with `!`
    // (e.g., "Teen Titans Go!") won't accidentally trigger archive extraction.
    if let Some(response) = serve_plain_file(absolute_path, request).await {
        return response;
    }

    // File doesn't exist directly — try archive extraction if the path looks
    // archive-nested (archive extension followed by `!` delimiter)
    if archive::is_archive_nested(absolute_path) {
        // Check if this is a distributed split RAR (volumes in different archives)
        if let Some(distributed_sources) = &file_info.distributed_rar_sources
            && !distributed_sources.is_empty()
        {
            match archive::stream_from_distributed_rar(
                absolute_path,
                distributed_sources,
                state.unrar_path.as_deref(),
                state.sevenz_path.as_deref(),
            )
            .await
            {
                Ok(stream) => return archive_response(stream, absolute_path),
                Err(err) => return archive_error_response(&err, absolute_path, "Distributed RAR extraction failed"),
            }
        }

        // Check if this is a distributed numeric split (.7z.001, .zip.001, etc.)
        if let Some(distributed_sources) = &file_info.distributed_numeric_sources
            && !distributed_sources.is_empty()
        {
            match archive::stream_from_distributed_numeric_split(
                absolute_path,
                distributed_sources,
                file_info.numeric_split_type.as_deref(),
                state.unrar_path.as_deref(),
                state.sevenz_path.as_deref(),
            )
            .await
            {
                Ok(stream) => return archive_response(stream, absolute_path),
                Err(err) => return archive_error_response(&err, absolute_path, "Distributed numeric split extraction failed"),
            }
        }

        // Count nesting levels
        let delimiter_count = archive::count_archive_delimiters(absolute_path);

        if delimiter_count > 1 {
            // Multi-level: e.g., ZIP!/RAR!/file.epub
            // Use the nested extraction function that handles split RAR reassembly
            match archive::stream_nested_file(
                absolute_path,
                file_info.split_rar_volumes.as_deref(),
                state.unrar_path.as_deref(),
                state.sevenz_path.as_deref(),
            )
            .await
            {
                Ok(stream) => return archive_response(stream, absolute_path),
                Err(err) => return archive_error_response(&err, absolute_path, "Nested archive extraction failed"),
            }
        }

        // Single level extraction
        if let Some((archive_part, inner_path)) = archive::split_archive_path(absolute_path) {
            match archive::stream_file(
                archive_part,
                inner_path,
                state.unrar_path.as_deref(),
                state.sevenz_path.as_deref(),
            )
            .await
            {
                Ok(stream) => return archive_response(stream, absolute_path),
                Err(err) => {
                    return archive_error_response(&err, absolute_path, "Archive extraction failed");
                }
            }
        }
    }

    (StatusCode::NOT_FOUND, "file not found on disk").into_response()
}

/// Archive entries are streamed after the 200 is sent, so a failure part-way
/// can only abort the connection. Log it, or a truncated transfer is silent.
/// Entries can't be seeked, so any Range header is ignored and the whole entry
/// sent (which HTTP permits); `Accept-Ranges: none` tells clients not to try.
fn archive_response(stream: archive::EntryStream, path: &str) -> Response {
    let content_length = stream.content_length();
    let path = path.to_string();
    let body = Body::from_stream(stream.inspect_err(move |err| {
        warn!(error = %err, path = %path, "Archive extraction failed mid-stream");
    }));

    let mut response = (StatusCode::OK, body).into_response();
    let headers = response.headers_mut();
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("none"));
    if let Some(len) = content_length {
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    }
    response
}

fn archive_error_response(err: &std::io::Error, path: &str, context: &str) -> Response {
    warn!(error = %err, path = %path, "{context}");
    match err.kind() {
        std::io::ErrorKind::ResourceBusy => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "30")],
            "too many archive extractions in progress",
        )
            .into_response(),
        std::io::ErrorKind::StorageFull => (
            StatusCode::SERVICE_UNAVAILABLE,
            "not enough temp space to extract file",
        )
            .into_response(),
        _ => (StatusCode::NOT_FOUND, "file not found in archive").into_response(),
    }
}

pub async fn stream_downloads(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> impl IntoResponse {
    if require_auth_with_token(&state, &headers, query.token.as_deref())
        .await
        .is_err()
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Get the caller's API key ID for filtering (None if using env var key or session)
    let caller_api_key_id = crate::auth::get_api_key_id(&state, &headers).await;

    // Fetch initial snapshot (recent downloads for SSE), filtered by API key if applicable
    let options = downloads_repo::DownloadListOptions {
        limit: 100, // Reasonable limit for SSE snapshot
        cursor: None,
        status: None,
        search: None,
    };
    let initial_downloads = match downloads_repo::fetch_download_list(&state.pool, options).await {
        Ok(response) => {
            if let Some(key_id) = caller_api_key_id {
                // Filter to only show downloads created by this API key
                response
                    .items
                    .into_iter()
                    .filter(|d| d.api_key_id == Some(key_id))
                    .collect()
            } else {
                // No API key filtering (env var key or session sees all)
                response.items
            }
        }
        Err(err) => {
            warn!(error = %err, "Failed to fetch initial downloads for stream");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let initial_event = DownloadStreamEvent::Snapshot {
        downloads: initial_downloads,
    };

    // Subscribe to broadcast channel for future events
    let rx = state.download_events.subscribe();

    // Create a stream that yields the initial snapshot, then broadcasts
    let initial_stream = tokio_stream::once(Ok::<_, Infallible>(sse_event_from_download_event(
        initial_event,
    )));

    // Filter broadcast events by API key if applicable
    let broadcast_stream = BroadcastStream::new(rx).filter_map(move |result| match result {
        Ok(event) => {
            let filtered_event = if let Some(key_id) = caller_api_key_id {
                // Filter event to only include downloads from this API key
                filter_event_by_api_key(event, key_id)
            } else {
                Some(event)
            };
            filtered_event.map(|e| Ok::<_, Infallible>(sse_event_from_download_event(e)))
        }
        Err(_) => None, // Skip lagged messages
    });

    let combined_stream = initial_stream.chain(broadcast_stream);

    Sse::new(combined_stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(30)))
        .into_response()
}

/// Filter a download stream event to only include downloads matching the given API key ID.
/// Returns None if the event should be completely filtered out.
fn filter_event_by_api_key(
    event: DownloadStreamEvent,
    api_key_id: Uuid,
) -> Option<DownloadStreamEvent> {
    match event {
        DownloadStreamEvent::Snapshot { downloads } => {
            let filtered: Vec<_> = downloads
                .into_iter()
                .filter(|d| d.api_key_id == Some(api_key_id))
                .collect();
            Some(DownloadStreamEvent::Snapshot {
                downloads: filtered,
            })
        }
        DownloadStreamEvent::Created { download } => {
            if download.api_key_id == Some(api_key_id) {
                Some(DownloadStreamEvent::Created { download })
            } else {
                None
            }
        }
        DownloadStreamEvent::Updated { download } => {
            if download.api_key_id == Some(api_key_id) {
                Some(DownloadStreamEvent::Updated { download })
            } else {
                None
            }
        }
        // Stream warm events are not per-API-key — always pass through
        event @ DownloadStreamEvent::StreamWarmReady { .. } => Some(event),
    }
}

fn sse_event_from_download_event(event: DownloadStreamEvent) -> Event {
    let payload = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
    Event::default().data(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::DownloadListItem;
    use time::OffsetDateTime;

    fn make_download(id: Uuid, name: &str, api_key_id: Option<Uuid>) -> DownloadListItem {
        DownloadListItem {
            id,
            name: name.to_string(),
            protocol: "torrent".to_string(),
            profile_id: None,
            status: "pending".to_string(),
            progress: None,
            last_poll_at: None,
            last_poll_error: None,
            created_at: OffsetDateTime::now_utc(),
            updated_at: OffsetDateTime::now_utc(),
            download_client_id: None,
            external_id: None,
            file_path: None,
            error: None,
            callback_sent_at: None,
            import_confirmed_at: None,
            files_json: None,
            release_payload: None,
            release_interpretation_json: None,
            metadata: serde_json::json!({}),
            callback_attempts: 0,
            callback_last_attempt_at: None,
            callback_error: None,
            files_scan_error: None,
            client_progress: None,
            download_client_name: None,
            api_key_id,
            api_key_name: None,
            watch_pattern_source_system: None,
            watch_pattern_source_id: None,
        }
    }

    #[test]
    fn test_filter_snapshot_by_api_key() {
        let key1 = Uuid::new_v4();
        let key2 = Uuid::new_v4();

        let downloads = vec![
            make_download(Uuid::new_v4(), "Download 1", Some(key1)),
            make_download(Uuid::new_v4(), "Download 2", Some(key2)),
            make_download(Uuid::new_v4(), "Download 3", Some(key1)),
            make_download(Uuid::new_v4(), "Download 4", None), // No API key
        ];

        let event = DownloadStreamEvent::Snapshot { downloads };
        let filtered = filter_event_by_api_key(event, key1);

        match filtered {
            Some(DownloadStreamEvent::Snapshot { downloads }) => {
                assert_eq!(downloads.len(), 2);
                assert!(downloads.iter().all(|d| d.api_key_id == Some(key1)));
            }
            _ => panic!("Expected Snapshot event"),
        }
    }

    #[test]
    fn test_filter_created_matching_api_key() {
        let key1 = Uuid::new_v4();
        let download = make_download(Uuid::new_v4(), "Download 1", Some(key1));

        let event = DownloadStreamEvent::Created { download };
        let filtered = filter_event_by_api_key(event, key1);

        assert!(matches!(
            filtered,
            Some(DownloadStreamEvent::Created { .. })
        ));
    }

    #[test]
    fn test_filter_created_different_api_key() {
        let key1 = Uuid::new_v4();
        let key2 = Uuid::new_v4();
        let download = make_download(Uuid::new_v4(), "Download 1", Some(key2));

        let event = DownloadStreamEvent::Created { download };
        let filtered = filter_event_by_api_key(event, key1);

        assert!(filtered.is_none());
    }

    #[test]
    fn test_filter_updated_matching_api_key() {
        let key1 = Uuid::new_v4();
        let download = make_download(Uuid::new_v4(), "Download 1", Some(key1));

        let event = DownloadStreamEvent::Updated { download };
        let filtered = filter_event_by_api_key(event, key1);

        assert!(matches!(
            filtered,
            Some(DownloadStreamEvent::Updated { .. })
        ));
    }

    #[test]
    fn test_filter_updated_different_api_key() {
        let key1 = Uuid::new_v4();
        let key2 = Uuid::new_v4();
        let download = make_download(Uuid::new_v4(), "Download 1", Some(key2));

        let event = DownloadStreamEvent::Updated { download };
        let filtered = filter_event_by_api_key(event, key1);

        assert!(filtered.is_none());
    }

    #[test]
    fn test_filter_download_with_no_api_key() {
        let key1 = Uuid::new_v4();
        let download = make_download(Uuid::new_v4(), "Download 1", None);

        let event = DownloadStreamEvent::Created { download };
        let filtered = filter_event_by_api_key(event, key1);

        // Downloads without an API key should be filtered out
        assert!(filtered.is_none());
    }
}
