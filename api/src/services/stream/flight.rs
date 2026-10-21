use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use sqlx::PgPool;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::account_index;
use super::warm_cache::{UnavailableReason, reason_for_error};
use crate::debrid::{
    DebridError, DebridFile, DebridProvider, DebridProviderType, DebridTorrent,
    DebridTorrentStatus, ExistingTorrent, Lane, Throttled,
};

/// What kobbler's `classify` reads as `blocked`; keep "refused this" in it.
pub const REFUSED_MESSAGE: &str = "Real-Debrid refused this release. Try a different source.";

/// RD usually applies a selection before `selectFiles` returns; one short re-check
/// covers the times it has not, instead of always sleeping.
const SELECTION_RECHECK: Duration = Duration::from_millis(300);
/// A flight whose callers all went away keeps its result; past this age it is run again.
const STALE_FLIGHT: Duration = Duration::from_secs(120);
const CLAIM_TTL: Duration = Duration::from_secs(600);

#[derive(Debug, Clone)]
pub struct ReadyLink {
    pub stream_url: String,
    pub filename: String,
    pub filesize: i64,
    pub mime_type: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Acquired {
    Ready { link: ReadyLink, torrent_id: String },
    Pending { torrent_id: String, reused: bool },
    Refused,
    Failed { message: String },
}

#[derive(Clone)]
pub struct FlightContext {
    pub pool: PgPool,
    pub http_client: reqwest::Client,
    pub provider: Arc<dyn DebridProvider>,
    pub provider_type: DebridProviderType,
    pub api_key: String,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub provider_id: Uuid,
    pub info_hash: String,
    pub file_id: Option<i32>,
    pub file_name: Option<String>,
    pub magnet: String,
    pub release_title: Option<String>,
    /// A download fetches every link itself; only a stream needs one unrestricted here.
    pub want_link: bool,
}

impl Job {
    /// Hash-index file indexes count the torrent's files from 0; Real-Debrid's ids from 1.
    pub fn provider_file_id(&self, provider: DebridProviderType) -> Option<i32> {
        self.file_id
            .filter(|_| provider == DebridProviderType::RealDebrid)
            .map(|idx| idx + 1)
    }
}

type FlightKey = (Uuid, String);

struct Flight {
    id: u64,
    file_id: Option<i32>,
    want_link: bool,
    started: Instant,
    lane: Lane,
    outcome: Shared<BoxFuture<'static, Acquired>>,
}

static FLIGHTS: LazyLock<DashMap<FlightKey, Flight>> = LazyLock::new(DashMap::new);
static NEXT_ID: AtomicU64 = AtomicU64::new(0);
static CLAIMS: LazyLock<DashMap<FlightKey, Instant>> = LazyLock::new(DashMap::new);

/// Marks a hash as wanted by a user, so warming never deletes the torrent behind it.
pub fn claim(provider_id: Uuid, info_hash: &str) {
    CLAIMS.retain(|_, at| at.elapsed() < CLAIM_TTL);
    CLAIMS.insert((provider_id, info_hash.to_string()), Instant::now());
}

fn is_claimed(provider_id: Uuid, info_hash: &str) -> bool {
    CLAIMS
        .get(&(provider_id, info_hash.to_string()))
        .is_some_and(|at| at.elapsed() < CLAIM_TTL)
}

enum Step {
    Join(Shared<BoxFuture<'static, Acquired>>),
    Wait(Shared<BoxFuture<'static, Acquired>>),
    Lead(Shared<BoxFuture<'static, Acquired>>),
}

/// Gets a torrent on the account to the point of a stream URL, or as far as it will go.
/// One flight per provider and hash: a resolve, a warm probe and a download grab of the
/// same release share the add/select/unrestrict, and work that wants something different
/// (another file of the pack, or no link) waits its turn rather than racing a second add.
pub async fn acquire(ctx: &FlightContext, job: Job, lane: Lane) -> Acquired {
    let key = (job.provider_id, job.info_hash.clone());
    loop {
        let step = match FLIGHTS.entry(key.clone()) {
            dashmap::Entry::Occupied(mut e) => {
                if e.get().started.elapsed() > STALE_FLIGHT {
                    let (flight, outcome) = launch(ctx, &job, &lane);
                    e.insert(flight);
                    Step::Lead(outcome)
                } else {
                    let flight = e.get();
                    if lane.is_user() {
                        flight.lane.promote();
                    }
                    if flight.file_id == job.file_id && flight.want_link == job.want_link {
                        Step::Join(flight.outcome.clone())
                    } else {
                        Step::Wait(flight.outcome.clone())
                    }
                }
            }
            dashmap::Entry::Vacant(v) => {
                let (flight, outcome) = launch(ctx, &job, &lane);
                v.insert(flight);
                Step::Lead(outcome)
            }
        };
        match step {
            Step::Join(outcome) => {
                debug!(info_hash = %job.info_hash, "Joining in-flight debrid work");
                return outcome.await;
            }
            Step::Wait(outcome) => {
                outcome.await;
            }
            Step::Lead(outcome) => return outcome.await,
        }
    }
}

fn launch(
    ctx: &FlightContext,
    job: &Job,
    lane: &Lane,
) -> (Flight, Shared<BoxFuture<'static, Acquired>>) {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let key = (job.provider_id, job.info_hash.clone());
    let task = tokio::spawn(run(ctx.clone(), job.clone(), lane.clone()));
    let outcome = async move {
        let out = task.await.unwrap_or_else(|e| Acquired::Failed {
            message: format!("Debrid work aborted: {e}"),
        });
        FLIGHTS.remove_if(&key, |_, f| f.id == id);
        out
    }
    .boxed()
    .shared();
    let flight = Flight {
        id,
        file_id: job.file_id,
        want_link: job.want_link,
        started: Instant::now(),
        lane: lane.clone(),
        outcome: outcome.clone(),
    };
    (flight, outcome)
}

struct Run {
    ctx: FlightContext,
    job: Job,
    handler: Throttled,
}

async fn run(ctx: FlightContext, job: Job, lane: Lane) -> Acquired {
    let handler = Throttled::new(ctx.provider.clone(), &ctx.api_key, lane);
    Run { ctx, job, handler }.go().await
}

struct Target {
    name: Option<String>,
    id: Option<i32>,
}

type Placed = (String, bool, DebridTorrent);

impl Run {
    async fn go(&self) -> Acquired {
        let target = self.target().await;
        let placed = match self.obtain(&target).await {
            Ok(placed) => placed,
            Err(out) => return out,
        };
        let (torrent_id, reused, torrent) = match self.settle(placed, &target).await {
            Ok(placed) => placed,
            Err(out) => return out,
        };

        match torrent.status {
            DebridTorrentStatus::Blocked => {
                self.record(Some(UnavailableReason::Blocked)).await;
                self.discard(&torrent_id, reused).await;
                Acquired::Refused
            }
            DebridTorrentStatus::Error { ref message } => {
                self.record(Some(UnavailableReason::Dead)).await;
                self.discard(&torrent_id, reused).await;
                Acquired::Failed {
                    message: format!("Torrent failed on provider: {message}"),
                }
            }
            DebridTorrentStatus::Cached | DebridTorrentStatus::Ready
                if !torrent.links.is_empty() =>
            {
                if self.job.want_link {
                    let link = torrent
                        .wanted_link(target.name.as_deref(), target.id)
                        .unwrap_or(&torrent.links[0]);
                    self.unrestrict(&torrent_id, link).await
                } else {
                    self.record(None).await;
                    Acquired::Pending { torrent_id, reused }
                }
            }
            _ => Acquired::Pending { torrent_id, reused },
        }
    }

    async fn target(&self) -> Target {
        let name = match &self.job.file_name {
            Some(name) => Some(name.clone()),
            None => self.indexed_file_name().await,
        };
        Target {
            name,
            id: self.job.provider_file_id(self.ctx.provider_type),
        }
    }

    /// A torrent on the account is reused only if it already serves the wanted file;
    /// otherwise the hash is added again, since RD lets a torrent select files only once.
    async fn obtain(&self, target: &Target) -> Result<Placed, Acquired> {
        let Some(found) = account_index::lookup(
            self.job.provider_id,
            &self.handler,
            &self.ctx.http_client,
            &self.ctx.api_key,
            &self.job.info_hash,
        )
        .await
        else {
            return self.add_and_get().await;
        };
        match self.get(&found.id).await {
            Ok(t) if t.serves(target.name.as_deref(), target.id) => {
                debug!(info_hash = %self.job.info_hash, torrent_id = %found.id, "Reusing existing torrent");
                Ok((found.id, true, t))
            }
            Ok(_) => {
                info!(info_hash = %self.job.info_hash, torrent_id = %found.id, "Existing torrent lacks the wanted file; adding it again");
                self.add_and_get().await
            }
            Err(DebridError::NotFound) => {
                account_index::forget(self.job.provider_id, &self.job.info_hash);
                self.add_and_get().await
            }
            Err(e) => Err(self
                .failed(None, false, "Failed to get torrent status", e)
                .await),
        }
    }

    /// A stream from a pack selects every episode, so the next ones are ready in the same
    /// torrent. If that makes RD download (only some episodes cached), a torrent this
    /// flight added is replaced by one holding just the wanted file.
    async fn settle(&self, placed: Placed, target: &Target) -> Result<Placed, Acquired> {
        let (torrent_id, reused, torrent) = placed;
        if !matches!(torrent.status, DebridTorrentStatus::WaitingFileSelection) {
            return Ok((torrent_id, reused, torrent));
        }
        let single: Vec<i32> =
            DebridFile::wanted(&torrent.files, target.name.as_deref(), target.id)
                .map(|f| vec![f.id])
                .unwrap_or_default();
        let pack = DebridFile::pack_select_ids(&torrent.files);
        if !self.job.want_link || pack.len() < 2 {
            let torrent = self.select(&torrent_id, reused, &single).await?;
            return Ok((torrent_id, reused, torrent));
        }
        let torrent = self.select(&torrent_id, reused, &pack).await?;
        let partial = matches!(
            torrent.status,
            DebridTorrentStatus::Downloading { .. } | DebridTorrentStatus::WaitingFileSelection
        );
        if !partial || reused {
            return Ok((torrent_id, reused, torrent));
        }
        info!(info_hash = %self.job.info_hash, "Pack is not wholly cached; selecting just the wanted file");
        self.discard(&torrent_id, false).await;
        let (torrent_id, _, _) = self.add_and_get().await?;
        let torrent = self.select(&torrent_id, false, &single).await?;
        Ok((torrent_id, false, torrent))
    }

    async fn get(&self, torrent_id: &str) -> Result<DebridTorrent, DebridError> {
        self.handler
            .get_torrent(&self.ctx.http_client, &self.ctx.api_key, torrent_id)
            .await
    }

    async fn add_and_get(&self) -> Result<Placed, Acquired> {
        info!(info_hash = %self.job.info_hash, "Adding magnet to debrid provider");
        let id = match self
            .handler
            .add_magnet(&self.ctx.http_client, &self.ctx.api_key, &self.job.magnet)
            .await
        {
            Ok(id) => id,
            Err(DebridError::Blocked) => {
                self.record(Some(UnavailableReason::Blocked)).await;
                return Err(Acquired::Refused);
            }
            Err(e) => return Err(self.failed(None, false, "Failed to add magnet", e).await),
        };
        account_index::record_added(
            self.job.provider_id,
            &self.job.info_hash,
            ExistingTorrent {
                id: id.clone(),
                status: DebridTorrentStatus::WaitingFileSelection,
                has_links: false,
            },
        );
        match self.get(&id).await {
            Ok(t) => Ok((id, false, t)),
            Err(e) => Err(self
                .failed(Some(&id), false, "Failed to get torrent status", e)
                .await),
        }
    }

    async fn select(
        &self,
        torrent_id: &str,
        reused: bool,
        ids: &[i32],
    ) -> Result<DebridTorrent, Acquired> {
        info!(info_hash = %self.job.info_hash, select_ids = ?ids, "Selecting files");
        if let Err(e) = self
            .handler
            .select_files(&self.ctx.http_client, &self.ctx.api_key, torrent_id, ids)
            .await
        {
            warn!(info_hash = %self.job.info_hash, error = %e, "select_files failed");
        }
        let mut torrent = self.get(torrent_id).await;
        if matches!(&torrent, Ok(t) if matches!(t.status, DebridTorrentStatus::WaitingFileSelection))
        {
            tokio::time::sleep(SELECTION_RECHECK).await;
            torrent = self.get(torrent_id).await;
        }
        match torrent {
            Ok(t) => Ok(t),
            Err(e) => Err(self
                .failed(Some(torrent_id), reused, "Failed to get torrent status", e)
                .await),
        }
    }

    async fn unrestrict(&self, torrent_id: &str, link: &str) -> Acquired {
        match self
            .handler
            .unrestrict_link(&self.ctx.http_client, &self.ctx.api_key, link)
            .await
        {
            Ok(u) => {
                self.record(None).await;
                Acquired::Ready {
                    link: ReadyLink {
                        stream_url: u.url,
                        filename: u.filename,
                        filesize: u.filesize,
                        mime_type: u.mime_type,
                        expires_at: u.expires_at.map(|t| {
                            t.format(&time::format_description::well_known::Rfc3339)
                                .unwrap_or_default()
                        }),
                    },
                    torrent_id: self.ctx.provider_type.prefixed_id(torrent_id),
                }
            }
            Err(e) => {
                if let Some(reason) = reason_for_error(&e) {
                    self.record(Some(reason)).await;
                }
                Acquired::Failed {
                    message: format!("Failed to unrestrict link: {e}"),
                }
            }
        }
    }

    async fn failed(
        &self,
        torrent_id: Option<&str>,
        reused: bool,
        context: &str,
        e: DebridError,
    ) -> Acquired {
        if let Some(reason) = reason_for_error(&e) {
            self.record(Some(reason)).await;
        }
        if let Some(id) = torrent_id {
            self.discard(id, reused).await;
        }
        warn!(info_hash = %self.job.info_hash, error = %e, "{context}");
        Acquired::Failed {
            message: format!("{context}: {e}"),
        }
    }

    /// Only what this flight added is deleted: a reused entry may be a download someone wants.
    async fn discard(&self, torrent_id: &str, reused: bool) {
        if reused {
            return;
        }
        account_index::forget(self.job.provider_id, &self.job.info_hash);
        if let Err(e) = self
            .handler
            .delete_torrent(&self.ctx.http_client, &self.ctx.api_key, torrent_id)
            .await
        {
            debug!(torrent_id = %torrent_id, error = %e, "Cleanup delete failed");
        }
    }

    async fn record(&self, unavailable: Option<UnavailableReason>) {
        let title = match &self.job.release_title {
            Some(t) => Some(t.clone()),
            None => crate::repositories::hash_index::get_original_title(
                &self.ctx.pool,
                &self.job.info_hash,
            )
            .await
            .unwrap_or_else(|e| {
                warn!(info_hash = %self.job.info_hash, error = %e, "Failed to look up release title");
                None
            }),
        };
        super::outcomes::record_probe(
            &self.ctx.pool,
            self.job.provider_id,
            &self.job.info_hash,
            title.as_deref(),
            unavailable,
        )
        .await;
    }

    async fn indexed_file_name(&self) -> Option<String> {
        let file_idx = self.job.file_id?;
        crate::repositories::hash_index::get_file_name(
            &self.ctx.pool,
            &self.job.info_hash,
            file_idx,
        )
        .await
        .unwrap_or_else(|e| {
            warn!(info_hash = %self.job.info_hash, file_idx, error = %e, "Failed to look up file name");
            None
        })
    }
}

/// Removes a torrent a probe added, unless a user is waiting on it.
pub async fn release_probe(ctx: &FlightContext, job: &Job, torrent_id: &str) {
    if is_claimed(job.provider_id, &job.info_hash) {
        return;
    }
    account_index::forget(job.provider_id, &job.info_hash);
    let handler = Throttled::new(ctx.provider.clone(), &ctx.api_key, Lane::background());
    if let Err(e) = handler
        .delete_torrent(&ctx.http_client, &ctx.api_key, torrent_id)
        .await
    {
        debug!(torrent_id = %torrent_id, error = %e, "Warm cache: cleanup delete failed");
    }
}
