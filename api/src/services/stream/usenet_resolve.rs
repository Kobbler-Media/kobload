use std::sync::Arc;

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::attempt_log::{AttemptLog, Origin, Started, Step};
use super::resolve::{StreamResolveContext, lane};
use super::service::StreamSearchError;
use super::usenet_flight::{self, ImportJob};
use super::{usenet_listing, usenet_warm};
use crate::debrid::{DebridProvider, Throttled};
use crate::repositories::stream_attempts::Change;
use crate::repositories::usenet_releases::{self, UsenetRelease};
use crate::usenet::outputs::{self, Target};
use crate::usenet::{STREAM_TTL, UsenetEngine};
use nats_jobs::NzbRef;
use nats_jobs::types::{StreamResolveRequest, StreamResolveResponse};
use usenet_proto::{ImportResponse, OutputInfo, Purpose};

const WARM_SKIPPED: &str =
    "Not warmed: only the release a search would auto-play is imported ahead of a tap";

fn failed(message: impl Into<String>) -> StreamSearchError {
    StreamSearchError::DebridError(message.into())
}

fn database(e: impl std::fmt::Display) -> StreamSearchError {
    StreamSearchError::Database(e.to_string())
}

/// Imports the NZB into our engine (the indexer is asked for it only when the engine has no
/// copy), picks the file the caller wants and hands back signed engine URLs.
pub async fn resolve(
    ctx: &StreamResolveContext<'_>,
    provider: Arc<dyn DebridProvider>,
    api_key: &str,
    request: &StreamResolveRequest,
) -> Result<StreamResolveResponse, StreamSearchError> {
    let engine = ctx
        .debrid_providers
        .usenet_engine()
        .ok_or_else(|| failed("The usenet engine is not configured"))?;
    let (nzb_ref, release) = listed(ctx, request).await?;
    let lane = lane(request);
    if !lane.is_user() && !usenet_warm::may_warm(request.debrid_provider_id, &nzb_ref) {
        return Err(failed(WARM_SKIPPED));
    }
    let title = request
        .release_title
        .clone()
        .or_else(|| release.release_title.clone())
        .unwrap_or_else(|| nzb_ref.guid.clone());
    let cache_key = format!("{}/{}", nzb_ref.indexer_id, nzb_ref.guid);
    let log = AttemptLog::start(
        ctx.pool,
        Started {
            provider_id: request.debrid_provider_id,
            origin: if lane.is_user() {
                Origin::Play
            } else {
                Origin::Warm
            },
            release_key: release.release_key.as_deref().unwrap_or(&cache_key),
            file_id: request.file_id,
            release_title: Some(&title),
            file_name: request.file_name.as_deref(),
        },
    )
    .await;

    let handler = Throttled::new(provider, api_key, lane.clone());
    let _session = lane.is_user().then(|| handler.gate().user_session());
    let job = ImportJob {
        pool: ctx.pool.clone(),
        http_client: ctx.http_client.clone(),
        provider_id: request.debrid_provider_id,
        handler: Arc::new(handler),
        nzb_ref: nzb_ref.clone(),
        download_url: release.download_url.clone(),
        name: title,
        known_key: release.release_key.clone(),
    };
    let flight = usenet_flight::import(job, lane.clone()).await;
    if flight.joined {
        log.record(
            Step::Joined,
            "Joined an import already in flight for this NZB",
            Change::default(),
        )
        .await;
    }
    let imported = match flight.imported {
        Ok(imported) => imported,
        Err(failure) => {
            log.record(
                Step::Failed,
                &failure.message,
                Change::failed(&failure.message),
            )
            .await;
            return Err(failed(failure.message));
        }
    };

    let target = target(request, &nzb_ref);
    let Some(output) = outputs::choose(&imported.outputs, request.file_id, &target) else {
        let message = format!("{} holds no file to play", imported.release_key);
        log.record(Step::Failed, &message, Change::failed(&message))
            .await;
        return Err(failed(message));
    };
    let purpose = if lane.is_user() {
        Purpose::Cache
    } else {
        Purpose::Warm
    };
    let response = response(&engine, &imported, output, purpose);
    log.record(
        Step::Ready,
        format!("Stream ready: {}", response.filename),
        Change {
            status: Some("ready"),
            cached: Some(true),
            progress: Some(1.0),
            release_key: Some(imported.release_key.clone()),
            ..Default::default()
        },
    )
    .await;
    Ok(response)
}

/// A binge or a re-resolve may know the release only by its key; any listing of it serves.
async fn listed(
    ctx: &StreamResolveContext<'_>,
    request: &StreamResolveRequest,
) -> Result<(NzbRef, UsenetRelease), StreamSearchError> {
    let refs = match (&request.nzb_ref, &request.release_key) {
        (Some(nzb_ref), _) => vec![nzb_ref.clone()],
        (None, Some(key)) => usenet_releases::aliases(ctx.pool, key)
            .await
            .map_err(database)?,
        (None, None) => Vec::new(),
    };
    for nzb_ref in refs {
        if let Some(release) = usenet_releases::get(ctx.pool, &nzb_ref)
            .await
            .map_err(database)?
        {
            return Ok((nzb_ref, release));
        }
    }
    Err(failed("This NZB is not listed any more; search again"))
}

fn target(request: &StreamResolveRequest, nzb_ref: &NzbRef) -> Target {
    let by_title = |t: &Option<String>| t.as_deref().map(Target::from_title);
    [
        by_title(&request.file_name),
        by_title(&request.release_title),
    ]
    .into_iter()
    .flatten()
    .find(|t| *t != Target::Largest)
    .or_else(|| usenet_listing::target(nzb_ref))
    .unwrap_or(Target::Largest)
}

fn response(
    engine: &UsenetEngine,
    imported: &ImportResponse,
    output: &OutputInfo,
    cache_purpose: Purpose,
) -> StreamResolveResponse {
    let expires_at = OffsetDateTime::now_utc() + STREAM_TTL;
    let expiry = expires_at.unix_timestamp();
    let key = &imported.release_key;
    StreamResolveResponse {
        stream_url: engine.stream_url(key, output.index, Purpose::Play, expiry),
        cache_url: Some(engine.stream_url(key, output.index, cache_purpose, expiry)),
        filename: outputs::filename(output),
        filesize: i64::try_from(output.size).unwrap_or(i64::MAX),
        mime_type: None,
        expires_at: expires_at.format(&Rfc3339).ok(),
        cached: true,
        torrent_id: None,
        release_key: Some(key.clone()),
    }
}
