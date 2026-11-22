use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use dashmap::DashMap;
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

use super::outcomes::{record_import, record_probe};
use super::usenet_failure::import_failed;
use super::usenet_files;
use super::usenet_nzb::{self, FetchError};
use super::warm_cache::UnavailableReason;
use crate::debrid::{DebridError, DebridProvider, Lane};
use crate::repositories::usenet_releases;
use nats_jobs::NzbRef;
use usenet_proto::{ImportMode, ImportRequest, ImportResponse};

pub const WARM_SKIPPED: &str = "Not warmed: only the release a search would auto-play is grabbed from the indexer ahead of a tap";
/// Long enough that a burst of taps and warms on a refused NZB grabs it once.
const FETCH_FAILURE_MINUTES: i32 = 15;

#[derive(Debug, Clone)]
pub struct ImportFailure {
    pub message: String,
}

pub type Imported = Result<Arc<ImportResponse>, ImportFailure>;

#[derive(Clone)]
pub struct ImportJob {
    pub pool: PgPool,
    pub http_client: reqwest::Client,
    pub provider_id: Uuid,
    pub handler: Arc<dyn DebridProvider>,
    pub nzb_ref: NzbRef,
    pub download_url: String,
    pub name: String,
    pub known_key: Option<String>,
    pub fetch_failure: Option<String>,
    /// Whether the indexer may be asked for the NZB when the engine has no copy.
    pub may_grab: bool,
    pub mode: ImportMode,
}

struct Flight {
    id: u64,
    lane: Lane,
    outcome: Shared<BoxFuture<'static, Imported>>,
}

type FlightKey = (Uuid, String, bool);

static FLIGHTS: LazyLock<DashMap<FlightKey, Flight>> = LazyLock::new(DashMap::new);
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

pub struct Joined {
    pub imported: Imported,
    pub joined: bool,
}

/// One import per provider and NZB at a time: a tap, a warm and a binge of the same release
/// share it, so the indexer is asked for the NZB at most once.
pub async fn import(job: ImportJob, lane: Lane) -> Joined {
    let key = (job.provider_id, job.nzb_ref.guid.clone(), job.may_grab);
    let (outcome, joined) = match FLIGHTS.entry(key.clone()) {
        dashmap::Entry::Occupied(e) => {
            if lane.is_user() {
                e.get().lane.promote();
            }
            (e.get().outcome.clone(), true)
        }
        dashmap::Entry::Vacant(v) => {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            // The entry goes when the import ends, not when a caller polls it: a caller that
            // hung up would otherwise leave a finished flight for the next tap to join.
            let task = tokio::spawn(async move {
                let out = run(job).await;
                FLIGHTS.remove_if(&key, |_, f| f.id == id);
                out
            });
            let outcome = async move {
                task.await.unwrap_or_else(|e| {
                    Err(ImportFailure {
                        message: format!("Usenet import aborted: {e}"),
                    })
                })
            }
            .boxed()
            .shared();
            v.insert(Flight {
                id,
                lane,
                outcome: outcome.clone(),
            });
            (outcome, false)
        }
    };
    Joined {
        imported: outcome.await,
        joined,
    }
}

fn failure(message: impl Into<String>) -> ImportFailure {
    ImportFailure {
        message: message.into(),
    }
}

async fn run(job: ImportJob) -> Imported {
    let mut request = ImportRequest {
        cache_key: job.nzb_ref.guid.clone(),
        name: job.name.clone(),
        mode: job.mode,
        password: None,
        nzb_base64: None,
    };
    let mut fetched = false;
    let result = match job.handler.usenet_import(&request).await {
        Err(DebridError::NotFound) => {
            if !job.may_grab {
                return Err(failure(WARM_SKIPPED));
            }
            if let Some(remembered) = &job.fetch_failure {
                return Err(failure(remembered.clone()));
            }
            match usenet_nzb::fetch(&job.http_client, &job.download_url).await {
                Ok(nzb) => {
                    fetched = true;
                    request.nzb_base64 = Some(STANDARD.encode(nzb));
                    job.handler.usenet_import(&request).await
                }
                Err(e) => return Err(failure(fetch_failed(&job, &e).await)),
            }
        }
        other => other,
    };
    match result {
        Ok(imported) => {
            remember_import(&job, &imported, fetched).await;
            Ok(Arc::new(imported))
        }
        Err(e) => {
            let message = import_failed(&e);
            if invalid_nzb(&e) {
                remember_fetch_failure(&job, &message).await;
            } else if refused(&e) {
                remember_refusal(&job, &message).await;
            }
            Err(failure(message))
        }
    }
}

/// Only the engine's verdict on the release itself (missing articles, retention, a password)
/// counts against it; an unreachable engine or provider says nothing about the release.
fn refused(e: &DebridError) -> bool {
    matches!(e, DebridError::Status(status, _) if status.as_u16() == 422)
}

fn invalid_nzb(e: &DebridError) -> bool {
    matches!(e, DebridError::Status(status, code) if status.as_u16() == 400 && code == "invalid_nzb")
}

async fn fetch_failed(job: &ImportJob, error: &FetchError) -> String {
    let message = error.message();
    if error.remembered() {
        remember_fetch_failure(job, &message).await;
    }
    message
}

async fn remember_fetch_failure(job: &ImportJob, message: &str) {
    if let Err(e) = usenet_releases::record_fetch_failure(
        &job.pool,
        &job.nzb_ref,
        message,
        FETCH_FAILURE_MINUTES,
    )
    .await
    {
        warn!(error = %e, "Failed to remember a refused NZB grab");
    }
}

// An import the engine served from its own copy read no articles, so it is no news about
// the release's availability.
async fn remember_import(job: &ImportJob, imported: &ImportResponse, fetched: bool) {
    let key = &imported.release_key;
    if let Err(e) = usenet_releases::set_usenet_release_key(&job.pool, &job.nzb_ref, key).await {
        warn!(error = %e, release_key = %key, "Failed to remember an imported NZB's key");
    }
    usenet_files::store(key, imported.outputs.clone());
    if fetched {
        record_import(&job.pool, job.provider_id, key, Some(&job.name)).await;
    }
    info!(release_key = %key, outputs = imported.outputs.len(), "Usenet release imported");
}

async fn remember_refusal(job: &ImportJob, message: &str) {
    if let Err(e) = usenet_releases::record_failure(&job.pool, &job.nzb_ref, message).await {
        warn!(error = %e, "Failed to remember a refused usenet import");
    }
    if let Some(key) = &job.known_key {
        record_probe(
            &job.pool,
            job.provider_id,
            key,
            Some(&job.name),
            Some(UnavailableReason::Dead),
        )
        .await;
    }
}
