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

use super::outcomes::record_probe;
use super::usenet_files;
use super::warm_cache::UnavailableReason;
use crate::debrid::{DebridError, DebridProvider, Lane};
use crate::repositories::usenet_releases;
use nats_jobs::NzbRef;
use usenet_proto::{ImportMode, ImportRequest, ImportResponse};

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
}

impl ImportJob {
    pub fn cache_key(&self) -> String {
        format!("{}/{}", self.nzb_ref.indexer_id, self.nzb_ref.guid)
    }
}

struct Flight {
    id: u64,
    lane: Lane,
    outcome: Shared<BoxFuture<'static, Imported>>,
}

static FLIGHTS: LazyLock<DashMap<(Uuid, String), Flight>> = LazyLock::new(DashMap::new);
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

pub struct Joined {
    pub imported: Imported,
    pub joined: bool,
}

/// One import per provider and NZB at a time: a tap, a warm and a binge of the same release
/// share it, so the indexer is asked for the NZB at most once.
pub async fn import(job: ImportJob, lane: Lane) -> Joined {
    let key = (job.provider_id, job.cache_key());
    let (outcome, joined) = match FLIGHTS.entry(key.clone()) {
        dashmap::Entry::Occupied(e) => {
            if lane.is_user() {
                e.get().lane.promote();
            }
            (e.get().outcome.clone(), true)
        }
        dashmap::Entry::Vacant(v) => {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let task = tokio::spawn(run(job));
            let outcome = async move {
                let out = task.await.unwrap_or_else(|e| {
                    Err(ImportFailure {
                        message: format!("Usenet import aborted: {e}"),
                    })
                });
                FLIGHTS.remove_if(&key, |_, f| f.id == id);
                out
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

async fn run(job: ImportJob) -> Imported {
    let mut request = ImportRequest {
        cache_key: job.cache_key(),
        name: job.name.clone(),
        mode: ImportMode::Stream,
        password: None,
        nzb_base64: None,
    };
    let result = match job.handler.usenet_import(&request).await {
        Err(DebridError::NotFound) => match fetch_nzb(&job.http_client, &job.download_url).await {
            Ok(nzb) => {
                request.nzb_base64 = Some(STANDARD.encode(nzb));
                job.handler.usenet_import(&request).await
            }
            Err(message) => {
                return Err(ImportFailure { message });
            }
        },
        other => other,
    };
    match result {
        Ok(imported) => {
            remember_import(&job, &imported).await;
            Ok(Arc::new(imported))
        }
        Err(e) => {
            let message = format!("Usenet import failed: {e}");
            if refused(&e) {
                remember_refusal(&job, &message).await;
            }
            Err(ImportFailure { message })
        }
    }
}

/// Only the engine's verdict on the release itself (missing articles, retention, a password)
/// counts against it; an unreachable engine or provider says nothing about the release.
fn refused(e: &DebridError) -> bool {
    matches!(e, DebridError::Status(status, _) if status.as_u16() == 422)
}

async fn remember_import(job: &ImportJob, imported: &ImportResponse) {
    let key = &imported.release_key;
    if let Err(e) = usenet_releases::set_usenet_release_key(
        &job.pool,
        &job.nzb_ref.indexer_id,
        &job.nzb_ref.guid,
        key,
    )
    .await
    {
        warn!(error = %e, release_key = %key, "Failed to remember an imported NZB's key");
    }
    usenet_files::store(key, imported.outputs.clone());
    record_probe(&job.pool, job.provider_id, key, Some(&job.name), None).await;
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

/// The URL carries the indexer's API key, so neither it nor reqwest's error (which quotes
/// it) is ever reported.
async fn fetch_nzb(http_client: &reqwest::Client, download_url: &str) -> Result<Vec<u8>, String> {
    let resp = http_client.get(download_url).send().await.map_err(|e| {
        format!(
            "Could not fetch the NZB from the indexer: {}",
            e.without_url()
        )
    })?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("The indexer answered {status} for the NZB"));
    }
    resp.bytes().await.map(|b| b.to_vec()).map_err(|e| {
        format!(
            "Could not read the NZB from the indexer: {}",
            e.without_url()
        )
    })
}
