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
use super::attempt_log::{AttemptLog, Step};
use super::warm_cache::{UnavailableReason, reason_for_error};
use crate::debrid::magnet;
use crate::debrid::{
    DebridError, DebridFile, DebridProvider, DebridProviderType, DebridTorrent,
    DebridTorrentStatus, ExistingTorrent, Lane, Throttled, UnrestrictedLink,
};
use crate::repositories::cached_files;
use crate::repositories::stream_attempts::Change;
use crate::repositories::stream_torrents;

/// What kobbler's `classify` reads as `blocked`; keep "refused this" in it.
pub const REFUSED_MESSAGE: &str = "Real-Debrid refused this release. Try a different source.";

/// RD usually applies a selection before `selectFiles` returns; one short re-check
/// covers the times it has not, instead of always sleeping.
const SELECTION_RECHECK: Duration = Duration::from_millis(300);
/// A flight whose callers all went away keeps its result; past this age it is run again.
const STALE_FLIGHT: Duration = Duration::from_secs(120);
const CLAIM_TTL: Duration = Duration::from_secs(600);
const FRESH_NOT_FOUND_RETRIES: u32 = 2;
const FRESH_NOT_FOUND_DELAY: Duration = Duration::from_millis(1000);
/// A stream left downloading this long without anyone polling it may give up its slot.
const RECLAIM_AFTER_MINUTES: i32 = 30;

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
    pub log: AttemptLog,
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

enum Flow {
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
                    Flow::Lead(outcome)
                } else {
                    let flight = e.get();
                    if lane.is_user() {
                        flight.lane.promote();
                    }
                    if flight.file_id == job.file_id && flight.want_link == job.want_link {
                        Flow::Join(flight.outcome.clone())
                    } else {
                        Flow::Wait(flight.outcome.clone())
                    }
                }
            }
            dashmap::Entry::Vacant(v) => {
                let (flight, outcome) = launch(ctx, &job, &lane);
                v.insert(flight);
                Flow::Lead(outcome)
            }
        };
        match step {
            Flow::Join(outcome) => {
                debug!(info_hash = %job.info_hash, "Joining in-flight debrid work");
                job.log
                    .record(
                        Step::Joined,
                        "Joined work already in flight for this hash",
                        Change::default(),
                    )
                    .await;
                let out = outcome.await;
                job.log.finish(&out).await;
                return out;
            }
            Flow::Wait(outcome) => {
                outcome.await;
            }
            Flow::Lead(outcome) => return outcome.await,
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
    let mut handler = Throttled::new(ctx.provider.clone(), &ctx.api_key, lane);
    if super::vouched::is_vouched(job.provider_id, &job.info_hash) {
        handler = handler.trusting_name();
    }
    let run = Run { ctx, job, handler };
    let out = run.go().await;
    run.job.log.finish(&out).await;
    out
}

struct Target {
    name: Option<String>,
    id: Option<i32>,
}

type Placed = (String, bool, DebridTorrent);

impl Run {
    async fn go(&self) -> Acquired {
        let target = self.target().await;
        let mut placed = match self.obtain(&target).await {
            Ok(placed) => placed,
            Err(out) => return out,
        };
        let mut stale: Option<String> = None;
        let mut single_only = false;
        loop {
            let (torrent_id, reused, torrent) =
                match self.settle(placed, &target, single_only).await {
                    Ok(placed) => placed,
                    Err(out) => return out,
                };
            self.log_cache_check(&torrent).await;
            account_index::remember(self.job.provider_id, &torrent);
            account_index::placed(
                self.job.provider_id,
                &self.job.info_hash,
                ExistingTorrent {
                    id: torrent_id.clone(),
                    status: torrent.status.clone(),
                    has_links: !torrent.links.is_empty(),
                },
            );

            return match torrent.status {
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
                    if !self.job.want_link {
                        self.record(None).await;
                        self.remember_set(&torrent).await;
                        return Acquired::Pending { torrent_id, reused };
                    }
                    let link = torrent
                        .wanted_link(target.name.as_deref(), target.id)
                        .unwrap_or(&torrent.links[0]);
                    match self.unrestrict(link).await {
                        // RD drops single files from old copies while the hash stays
                        // cached; Torrentio recovers the same way, by adding it again.
                        Err(DebridError::HosterUnavailable) if reused => {
                            placed = match self
                                .replace(&torrent_id, "lost this file (hoster_unavailable)")
                                .await
                            {
                                Ok(placed) => placed,
                                Err(out) => return out,
                            };
                            stale = Some(torrent_id);
                            continue;
                        }
                        Ok(u)
                            if is_archive(&u.filename)
                                && !single_only
                                && torrent.files.iter().filter(|f| f.selected).count() > 1 =>
                        {
                            placed = match self
                                .replace(&torrent_id, "served its selection as an archive")
                                .await
                            {
                                Ok(placed) => placed,
                                Err(out) => return out,
                            };
                            if reused {
                                stale = Some(torrent_id);
                            } else {
                                self.discard(&torrent_id, false).await;
                            }
                            single_only = true;
                            continue;
                        }
                        served => {
                            let out = self.ready(&torrent_id, served).await;
                            if matches!(out, Acquired::Ready { .. }) {
                                self.remember_set(&torrent).await;
                            }
                            // Deleted only once the new copy plays: a refused re-add
                            // leaves the old one as the only source of its other files.
                            if let (Acquired::Ready { .. }, Some(stale)) = (&out, &stale) {
                                self.discard(stale, false).await;
                            }
                            out
                        }
                    }
                }
                DebridTorrentStatus::Cached | DebridTorrentStatus::Ready
                    if reused && self.job.want_link =>
                {
                    placed = match self
                        .replace(&torrent_id, "is finished but has no links")
                        .await
                    {
                        Ok(placed) => placed,
                        Err(out) => return out,
                    };
                    stale = Some(torrent_id);
                    continue;
                }
                _ => Acquired::Pending { torrent_id, reused },
            };
        }
    }

    async fn replace(&self, torrent_id: &str, why: &str) -> Result<Placed, Acquired> {
        info!(info_hash = %self.job.info_hash, torrent_id, why, "Copy on the account cannot stream; adding the hash again");
        self.job
            .log
            .record(
                Step::Readded,
                format!("Torrent {torrent_id} {why}; adding the hash again"),
                Change::default(),
            )
            .await;
        self.add_and_get().await
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

    /// A copy on the account is reused only if it already serves the wanted file; with
    /// none that does, the hash is added again, since RD lets a torrent select files only
    /// once. Copies can differ in what they selected, so each is checked.
    async fn obtain(&self, target: &Target) -> Result<Placed, Acquired> {
        let provider_id = self.job.provider_id;
        let copies = account_index::copies(
            provider_id,
            &self.handler,
            &self.ctx.http_client,
            &self.ctx.api_key,
            &self.job.info_hash,
        )
        .await;
        let serves = |t: &DebridTorrent| t.serves(target.name.as_deref(), target.id);
        for found in &copies {
            if account_index::details(provider_id, &found.id).is_some_and(|t| !serves(&t)) {
                continue;
            }
            match self.get(&found.id).await {
                Ok(t) if serves(&t) => {
                    debug!(info_hash = %self.job.info_hash, torrent_id = %found.id, "Reusing existing torrent");
                    self.log_placed(
                        Step::Reused,
                        format!(
                            "Torrent {} already on the account serves this file",
                            found.id
                        ),
                        &found.id,
                    )
                    .await;
                    self.touch(&found.id).await;
                    return Ok((found.id.clone(), true, t));
                }
                Ok(t) => account_index::remember(provider_id, &t),
                Err(DebridError::NotFound) => {
                    account_index::removed(provider_id, &self.job.info_hash, &found.id);
                }
                Err(e) => {
                    return Err(self
                        .failed(None, false, "Failed to get torrent status", e)
                        .await);
                }
            }
        }
        if !copies.is_empty() {
            info!(info_hash = %self.job.info_hash, copies = copies.len(), "No copy on the account holds the wanted file; adding it again");
        }
        self.add_and_get().await
    }

    /// A stream from a pack selects every episode, so the next ones are ready in the same
    /// torrent. If that makes RD download (only some episodes cached), a torrent this
    /// flight added is replaced by one holding just the wanted file.
    async fn settle(
        &self,
        placed: Placed,
        target: &Target,
        single_only: bool,
    ) -> Result<Placed, Acquired> {
        let (torrent_id, reused, torrent) = placed;
        if !matches!(torrent.status, DebridTorrentStatus::WaitingFileSelection) {
            return Ok((torrent_id, reused, torrent));
        }
        let single: Vec<i32> =
            DebridFile::wanted(&torrent.files, target.name.as_deref(), target.id)
                .map(|f| vec![f.id])
                .unwrap_or_default();
        let pack = DebridFile::pack_select_ids(&torrent.files);
        if single_only || !self.job.want_link || pack.len() < 2 {
            let torrent = self.select(&torrent_id, reused, &single).await?;
            return Ok((torrent_id, reused, torrent));
        }
        let known =
            cached_files::sets(&self.ctx.pool, self.job.provider_id, &self.job.info_hash).await;
        let wanted = single.first();
        let choice = match known
            .iter()
            .find(|set| wanted.is_some_and(|w| set.contains(w)))
        {
            Some(set) => set.clone(),
            // Only ever streamed in part: the whole pack would download and be deleted.
            None if !known.is_empty() && !single.is_empty() => single.clone(),
            None => pack,
        };
        if choice.len() < 2 {
            let torrent = self.select(&torrent_id, reused, &choice).await?;
            return Ok((torrent_id, reused, torrent));
        }
        let torrent = self.select(&torrent_id, reused, &choice).await?;
        let partial = matches!(
            torrent.status,
            DebridTorrentStatus::Downloading { .. } | DebridTorrentStatus::WaitingFileSelection
        );
        if !partial || reused {
            return Ok((torrent_id, reused, torrent));
        }
        info!(info_hash = %self.job.info_hash, "Pack is not wholly cached; selecting just the wanted file");
        self.job
            .log
            .record(
                Step::Readded,
                "Pack is not wholly cached; re-adding with just the wanted file",
                Change::default(),
            )
            .await;
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

    /// RD is not read-your-writes: a torrent just added or selected can 404 for a moment.
    async fn get_fresh(&self, torrent_id: &str) -> Result<DebridTorrent, DebridError> {
        let mut retries = 0;
        loop {
            match self.get(torrent_id).await {
                Err(DebridError::NotFound) if retries < FRESH_NOT_FOUND_RETRIES => {
                    retries += 1;
                    tokio::time::sleep(FRESH_NOT_FOUND_DELAY).await;
                }
                other => return other,
            }
        }
    }

    async fn add_and_get(&self) -> Result<Placed, Acquired> {
        info!(info_hash = %self.job.info_hash, "Adding magnet to debrid provider");
        let magnet = magnet::for_add(&self.job.magnet, self.title().await.as_deref());
        let mut reclaimed = false;
        let id = loop {
            match self
                .handler
                .add_magnet(&self.ctx.http_client, &self.ctx.api_key, &magnet)
                .await
            {
                Ok(id) => break id,
                Err(DebridError::Blocked | DebridError::TooBig) => {
                    self.record(Some(UnavailableReason::Blocked)).await;
                    return Err(Acquired::Refused);
                }
                Err(DebridError::TooManyActive) if !reclaimed && self.free_slot().await => {
                    reclaimed = true;
                }
                Err(e) => return Err(self.failed(None, false, "Failed to add magnet", e).await),
            }
        };
        self.log_placed(Step::Added, format!("Magnet added as torrent {id}"), &id)
            .await;
        self.touch(&id).await;
        account_index::placed(
            self.job.provider_id,
            &self.job.info_hash,
            ExistingTorrent {
                id: id.clone(),
                status: DebridTorrentStatus::WaitingFileSelection,
                has_links: false,
            },
        );
        match self.get_fresh(&id).await {
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
        let selected = self
            .handler
            .select_files(&self.ctx.http_client, &self.ctx.api_key, torrent_id, ids)
            .await;
        let message = match &selected {
            Ok(()) => format!("Selected files {ids:?}"),
            Err(e) => format!("Selecting files {ids:?} failed: {e}"),
        };
        self.job
            .log
            .record(Step::Selected, message, Change::default())
            .await;
        if let Err(e) = selected {
            warn!(info_hash = %self.job.info_hash, error = %e, "select_files failed");
        }
        let mut torrent = self.get_fresh(torrent_id).await;
        if matches!(&torrent, Ok(t) if matches!(t.status, DebridTorrentStatus::WaitingFileSelection))
        {
            tokio::time::sleep(SELECTION_RECHECK).await;
            torrent = self.get_fresh(torrent_id).await;
        }
        match torrent {
            Ok(t) => Ok(t),
            Err(e) => Err(self
                .failed(Some(torrent_id), reused, "Failed to get torrent status", e)
                .await),
        }
    }

    async fn unrestrict(&self, link: &str) -> Result<UnrestrictedLink, DebridError> {
        self.handler
            .unrestrict_link(&self.ctx.http_client, &self.ctx.api_key, link)
            .await
    }

    async fn ready(
        &self,
        torrent_id: &str,
        served: Result<UnrestrictedLink, DebridError>,
    ) -> Acquired {
        match served {
            Ok(u) if !is_video(&u) => {
                self.record(Some(UnavailableReason::Dead)).await;
                Acquired::Failed {
                    message: format!(
                        "Resolved file is not a video: {} ({})",
                        u.filename,
                        u.mime_type.as_deref().unwrap_or("unknown type")
                    ),
                }
            }
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
        account_index::removed(self.job.provider_id, &self.job.info_hash, torrent_id);
        let _ = stream_torrents::removed(&self.ctx.pool, self.job.provider_id, torrent_id).await;
        if let Err(e) = self
            .handler
            .delete_torrent(&self.ctx.http_client, &self.ctx.api_key, torrent_id)
            .await
        {
            debug!(torrent_id = %torrent_id, error = %e, "Cleanup delete failed");
        }
    }

    async fn remember_set(&self, torrent: &DebridTorrent) {
        let ids: Vec<i32> = torrent
            .files
            .iter()
            .filter(|f| f.selected)
            .map(|f| f.id)
            .collect();
        cached_files::remember(
            &self.ctx.pool,
            self.job.provider_id,
            &self.job.info_hash,
            &ids,
        )
        .await;
    }

    async fn title(&self) -> Option<String> {
        match &self.job.release_title {
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
        }
    }

    /// The account is out of active-torrent slots. The torrent a stream left downloading
    /// that no stream has come back for in longest makes way for this one.
    async fn free_slot(&self) -> bool {
        let provider_id = self.job.provider_id;
        let pool = &self.ctx.pool;
        let candidates =
            match stream_torrents::untouched_for(pool, provider_id, RECLAIM_AFTER_MINUTES).await {
                Ok(found) => found,
                Err(e) => {
                    warn!(error = %e, "Failed to list stream torrents");
                    return false;
                }
            };
        for (torrent_id, info_hash) in candidates {
            match self.get(&torrent_id).await {
                Ok(t)
                    if matches!(
                        t.status,
                        DebridTorrentStatus::Downloading { .. }
                            | DebridTorrentStatus::WaitingFileSelection
                    ) => {}
                Ok(_) | Err(DebridError::NotFound) => {
                    let _ = stream_torrents::removed(pool, provider_id, &torrent_id).await;
                    continue;
                }
                Err(_) => return false,
            }
            if self
                .handler
                .delete_torrent(&self.ctx.http_client, &self.ctx.api_key, &torrent_id)
                .await
                .is_err()
            {
                return false;
            }
            account_index::removed(provider_id, &info_hash, &torrent_id);
            let _ = stream_torrents::removed(pool, provider_id, &torrent_id).await;
            info!(
                torrent_id,
                info_hash, "Freed a torrent slot held by a stale stream"
            );
            self.job
                .log
                .record(
                    Step::Readded,
                    format!("No free torrent slot; deleted stale torrent {torrent_id}"),
                    Change::default(),
                )
                .await;
            return true;
        }
        false
    }

    async fn touch(&self, torrent_id: &str) {
        if !self.job.want_link {
            return;
        }
        if let Err(e) = stream_torrents::touched(
            &self.ctx.pool,
            self.job.provider_id,
            torrent_id,
            &self.job.info_hash,
        )
        .await
        {
            warn!(error = %e, "Failed to note a stream's torrent");
        }
    }

    async fn record(&self, unavailable: Option<UnavailableReason>) {
        let title = self.title().await;
        super::outcomes::record_probe(
            &self.ctx.pool,
            self.job.provider_id,
            &self.job.info_hash,
            title.as_deref(),
            unavailable,
        )
        .await;
    }

    async fn log_placed(&self, step: Step, message: String, torrent_id: &str) {
        let change = Change {
            torrent_id: Some(torrent_id.to_string()),
            ..Default::default()
        };
        self.job.log.record(step, message, change).await;
    }

    async fn log_cache_check(&self, torrent: &DebridTorrent) {
        let (cached, progress, message) = match &torrent.status {
            DebridTorrentStatus::Cached | DebridTorrentStatus::Ready
                if !torrent.links.is_empty() =>
            {
                (Some(true), Some(1.0), "Cached on the provider".to_string())
            }
            DebridTorrentStatus::Cached | DebridTorrentStatus::Ready => (
                Some(true),
                None,
                "Provider says ready but has no links yet".to_string(),
            ),
            DebridTorrentStatus::Downloading { progress } => (
                Some(false),
                Some(*progress),
                format!(
                    "Not cached: provider is downloading ({:.0}%)",
                    progress * 100.0
                ),
            ),
            DebridTorrentStatus::WaitingFileSelection => {
                (None, None, "Still waiting for file selection".to_string())
            }
            DebridTorrentStatus::Blocked => {
                (None, None, "Provider refused the torrent".to_string())
            }
            DebridTorrentStatus::Error { message } => {
                (None, None, format!("Provider reports an error: {message}"))
            }
        };
        let change = Change {
            cached,
            progress,
            ..Default::default()
        };
        self.job.log.record(Step::CacheCheck, message, change).await;
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

/// RD's mime type is a guess from the name, so only a name and type that both say
/// otherwise rule a file out.
fn is_video(link: &UnrestrictedLink) -> bool {
    let ext = link
        .filename
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_lowercase();
    crate::parsing::file::VIDEO_EXTENSIONS.contains(&ext.as_str())
        || link
            .mime_type
            .as_deref()
            .is_none_or(|m| m.starts_with("video/"))
}

fn is_archive(filename: &str) -> bool {
    let lower = filename.to_lowercase();
    [".rar", ".zip", ".7z", ".tar"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// Removes a torrent a probe added, unless a user is waiting on it.
pub async fn release_probe(ctx: &FlightContext, job: &Job, torrent_id: &str) {
    if is_claimed(job.provider_id, &job.info_hash) {
        return;
    }
    account_index::removed(job.provider_id, &job.info_hash, torrent_id);
    let handler = Throttled::new(ctx.provider.clone(), &ctx.api_key, Lane::background());
    if let Err(e) = handler
        .delete_torrent(&ctx.http_client, &ctx.api_key, torrent_id)
        .await
    {
        debug!(torrent_id = %torrent_id, error = %e, "Warm cache: cleanup delete failed");
    }
}
