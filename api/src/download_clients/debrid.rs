use std::path::Path;
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use dashmap::DashMap;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};
use uuid::Uuid;

use crate::debrid::{
    DebridFile, DebridProvider, DebridProviderResolver, DebridTorrentStatus, Lane, Throttled,
};
use crate::download_clients::{DownloadClientError, PollResult, ScannedFile, get_extension};
use crate::services::stream::attempt_log::AttemptLog;
use crate::services::stream::flight::{self, Acquired, FlightContext, Job};
use crate::types::{DownloadClientConfig, TestConnectionResult};

use super::registry::{DownloadClientCapabilities, DownloadClientHandler};

/// In-memory tracker for debrid downloads in progress.
/// Keyed by external_id string.
static DOWNLOAD_STATE: LazyLock<DashMap<String, DebridDownloadProgress>> =
    LazyLock::new(DashMap::new);

/// Progress state for a debrid download.
#[derive(Debug, Clone)]
enum DebridDownloadProgress {
    /// Download task spawned, files being downloaded.
    Downloading {
        completed_bytes: u64,
        total_bytes: u64,
    },
    /// All files downloaded successfully.
    Completed { files: Vec<ScannedFile> },
    /// Download failed.
    Failed { error: String },
}

/// Debrid download client — routes magnet downloads through a debrid provider
/// instead of a traditional torrent client.
///
/// Unlike Transmission/SABnzbd, `add_url()` returns immediately after submitting
/// the magnet to the debrid provider. The actual status tracking, file selection,
/// unrestricting, and downloading happens in `poll()`.
///
/// Config fields used:
/// - `api_key`: the debrid provider DB row ID (UUID string)
/// - `download_dir`: local directory to download files into
pub struct DebridHandler {
    pool: PgPool,
    http_client: reqwest::Client,
    debrid_providers: std::sync::Arc<dyn DebridProviderResolver>,
}

impl DebridHandler {
    pub fn new(
        pool: PgPool,
        http_client: reqwest::Client,
        debrid_providers: std::sync::Arc<dyn DebridProviderResolver>,
    ) -> Self {
        Self {
            pool,
            http_client,
            debrid_providers,
        }
    }

    fn provider_id(client: &DownloadClientConfig) -> Result<Uuid, DownloadClientError> {
        let raw = client
            .api_key
            .as_deref()
            .ok_or(DownloadClientError::MissingApiKey)?;
        Uuid::parse_str(raw)
            .map_err(|_| DownloadClientError::Message(format!("invalid debrid_provider_id: {raw}")))
    }

    /// The provider as stored, before throttling: the shared flight applies its own lane.
    async fn raw_provider(
        &self,
        provider_id: Uuid,
    ) -> Result<(Arc<dyn DebridProvider>, String), DownloadClientError> {
        let row = sqlx::query!(
            "SELECT provider_type, api_key FROM debrid_providers WHERE id = $1",
            provider_id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| DownloadClientError::Message(format!("database error: {e}")))?
        .ok_or_else(|| {
            DownloadClientError::Message(format!("debrid provider {provider_id} not found"))
        })?;

        let handler = self
            .debrid_providers
            .get_by_str(&row.provider_type)
            .ok_or_else(|| {
                DownloadClientError::Message(format!(
                    "unknown debrid provider type: {}",
                    row.provider_type
                ))
            })?;

        Ok((handler, row.api_key))
    }

    /// Every call on the account goes through its shared rate gate, in the download lane.
    async fn resolve_provider_by_id(
        &self,
        provider_id: Uuid,
    ) -> Result<(Arc<dyn DebridProvider>, String), DownloadClientError> {
        let (raw, api_key) = self.raw_provider(provider_id).await?;
        let throttled: Arc<dyn DebridProvider> =
            Arc::new(Throttled::new(raw, &api_key, Lane::download()));
        Ok((throttled, api_key))
    }

    /// Extract info_hash from a magnet URL.
    fn extract_info_hash(magnet: &str) -> Result<String, DownloadClientError> {
        let lower = magnet.to_lowercase();
        if let Some(pos) = lower.find("btih:") {
            let after = &magnet[pos + 5..];
            let hash = after
                .split('&')
                .next()
                .unwrap_or(after)
                .trim()
                .to_lowercase();
            if hash.is_empty() {
                return Err(DownloadClientError::Message(
                    "empty info_hash in magnet URL".to_string(),
                ));
            }
            Ok(hash)
        } else {
            Err(DownloadClientError::Message(
                "no info_hash found in magnet URL".to_string(),
            ))
        }
    }
}

/// Stored as external_id JSON — carries everything poll() needs to resume.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DebridExternalId {
    provider_id: Uuid,
    torrent_id: String,
    info_hash: String,
    download_dir: String,
}

#[async_trait]
impl DownloadClientHandler for DebridHandler {
    fn client_type(&self) -> &'static str {
        "debrid"
    }

    fn capabilities(&self) -> DownloadClientCapabilities {
        DownloadClientCapabilities {
            supports_add_url: true,
            supports_add_bytes: false,
        }
    }

    async fn add_url(
        &self,
        _http_client: &reqwest::Client,
        client: &DownloadClientConfig,
        url: &str,
    ) -> Result<String, DownloadClientError> {
        let provider_id = Self::provider_id(client)?;
        let (provider, api_key) = self.raw_provider(provider_id).await?;
        let info_hash = Self::extract_info_hash(url)?;
        let ctx = FlightContext {
            pool: self.pool.clone(),
            http_client: self.http_client.clone(),
            provider_type: provider.provider_type(),
            provider,
            api_key,
        };
        let job = Job {
            provider_id,
            info_hash: info_hash.clone(),
            file_id: None,
            file_name: None,
            magnet: url.to_string(),
            release_title: None,
            want_link: false,
            log: AttemptLog::off(),
        };
        // Shares the account listing, the rate gate and any in-flight add of the same hash
        // with streaming, and keeps warming from deleting the torrent this grab now owns.
        flight::claim(provider_id, &info_hash);
        info!(info_hash = %info_hash, "adding magnet to debrid");
        let torrent_id = match flight::acquire(&ctx, job, Lane::download()).await {
            Acquired::Pending { torrent_id, .. } | Acquired::Ready { torrent_id, .. } => torrent_id,
            Acquired::Refused => {
                return Err(DownloadClientError::Message(
                    "provider refused this file (infringing/filtered)".to_string(),
                ));
            }
            Acquired::Failed { message } => return Err(DownloadClientError::Message(message)),
        };

        let external = DebridExternalId {
            provider_id,
            torrent_id,
            info_hash,
            download_dir: client
                .download_dir
                .clone()
                .unwrap_or_else(|| "/tmp/debrid-dl".to_string()),
        };

        serde_json::to_string(&external)
            .map_err(|e| DownloadClientError::Message(format!("serialize external_id: {e}")))
    }

    async fn add_bytes(
        &self,
        _http_client: &reqwest::Client,
        _client: &DownloadClientConfig,
        _bytes: &[u8],
        _title: &str,
    ) -> Result<String, DownloadClientError> {
        Err(DownloadClientError::Message(
            "debrid download client does not support torrent file uploads, use a magnet URL"
                .to_string(),
        ))
    }

    async fn poll(
        &self,
        _http_client: &reqwest::Client,
        _client: &DownloadClientConfig,
        external_id: &str,
    ) -> Result<PollResult, DownloadClientError> {
        // Check if a background download is already tracked
        if let Some(state) = DOWNLOAD_STATE.get(external_id) {
            return match state.value() {
                DebridDownloadProgress::Downloading {
                    completed_bytes,
                    total_bytes,
                } => {
                    let progress = if *total_bytes > 0 {
                        *completed_bytes as f64 / *total_bytes as f64
                    } else {
                        0.5
                    };
                    Ok(PollResult::downloading(Some(progress)))
                }
                DebridDownloadProgress::Completed { files } => {
                    let files = files.clone();
                    let ext: DebridExternalId =
                        serde_json::from_str(external_id).unwrap_or_else(|_| DebridExternalId {
                            provider_id: Uuid::nil(),
                            torrent_id: String::new(),
                            info_hash: String::new(),
                            download_dir: String::new(),
                        });
                    drop(state);
                    DOWNLOAD_STATE.remove(external_id);
                    let client_data = serde_json::json!({
                        "download_dir": ext.download_dir,
                        "info_hash": ext.info_hash,
                        "total_files": files.len(),
                        "total_size": files.iter().map(|f| f.size).sum::<i64>(),
                    });
                    Ok(PollResult::completed_with_data(files, client_data))
                }
                DebridDownloadProgress::Failed { error } => {
                    let err = error.clone();
                    drop(state);
                    DOWNLOAD_STATE.remove(external_id);
                    Ok(PollResult::failed(err, None))
                }
            };
        }

        let ext: DebridExternalId = serde_json::from_str(external_id).map_err(|e| {
            DownloadClientError::Message(format!("invalid debrid external_id: {e}"))
        })?;

        let (provider, api_key) = self.resolve_provider_by_id(ext.provider_id).await?;

        let torrent = provider
            .get_torrent(&self.http_client, &api_key, &ext.torrent_id)
            .await
            .map_err(|e| DownloadClientError::Message(format!("get_torrent failed: {e}")))?;

        match &torrent.status {
            DebridTorrentStatus::WaitingFileSelection => {
                let select_ids = DebridFile::resolve_select_ids(&torrent.files, None);
                info!(
                    info_hash = %ext.info_hash,
                    select_ids = ?select_ids,
                    "Debrid poll: selecting files (WaitingFileSelection)"
                );
                if let Err(e) = provider
                    .select_files(&self.http_client, &api_key, &ext.torrent_id, &select_ids)
                    .await
                {
                    warn!(error = %e, "select_files retry failed");
                }
                Ok(PollResult::downloading(Some(0.0)))
            }

            DebridTorrentStatus::Downloading { progress } => {
                Ok(PollResult::downloading(Some(*progress as f64)))
            }

            DebridTorrentStatus::Cached | DebridTorrentStatus::Ready => {
                if torrent.links.is_empty() {
                    return Err(DownloadClientError::Message(
                        "debrid torrent ready but no links available".to_string(),
                    ));
                }

                // Spawn background download task — poll() returns immediately
                let ext_id_key = external_id.to_string();
                let links = torrent.links.clone();
                let http = self.http_client.clone();
                let provider = provider.clone();
                let api_key = api_key.clone();

                // Estimate total size from torrent files (best effort)
                let total_bytes: u64 = torrent.files.iter().map(|f| f.size as u64).sum();

                DOWNLOAD_STATE.insert(
                    ext_id_key.clone(),
                    DebridDownloadProgress::Downloading {
                        completed_bytes: 0,
                        total_bytes,
                    },
                );

                tokio::spawn(async move {
                    let result = download_all_links(
                        &http,
                        provider.as_ref(),
                        &api_key,
                        &links,
                        &ext.download_dir,
                        &ext.info_hash,
                        &ext_id_key,
                    )
                    .await;

                    match result {
                        Ok(files) if !files.is_empty() => {
                            DOWNLOAD_STATE
                                .insert(ext_id_key, DebridDownloadProgress::Completed { files });
                        }
                        Ok(_) => {
                            DOWNLOAD_STATE.insert(
                                ext_id_key,
                                DebridDownloadProgress::Failed {
                                    error: "all file downloads failed".to_string(),
                                },
                            );
                        }
                        Err(e) => {
                            DOWNLOAD_STATE.insert(
                                ext_id_key,
                                DebridDownloadProgress::Failed {
                                    error: e.to_string(),
                                },
                            );
                        }
                    }
                });

                Ok(PollResult::downloading(Some(0.5)))
            }

            DebridTorrentStatus::Blocked => Ok(PollResult::failed(
                "provider refused this file (infringing/filtered)".to_string(),
                None,
            )),

            DebridTorrentStatus::Error { message } => {
                Ok(PollResult::failed(format!("debrid error: {message}"), None))
            }
        }
    }

    async fn remove(
        &self,
        _http_client: &reqwest::Client,
        _client: &DownloadClientConfig,
        external_id: &str,
        delete_data: bool,
    ) -> Result<(), DownloadClientError> {
        // Clean up in-memory download state (stops progress tracking)
        DOWNLOAD_STATE.remove(external_id);

        // Try to parse as new JSON format; fall back to legacy file path
        if let Ok(ext) = serde_json::from_str::<DebridExternalId>(external_id) {
            if delete_data {
                let download_base = Path::new(&ext.download_dir).join(&ext.info_hash);
                if download_base.exists()
                    && let Err(e) = tokio::fs::remove_dir_all(&download_base).await
                {
                    warn!(
                        path = %download_base.display(),
                        error = %e,
                        "failed to delete debrid download directory"
                    );
                }
            }
        } else if delete_data {
            // Legacy: external_id is a file path
            let path = Path::new(external_id);
            if path.exists() {
                if let Err(e) = tokio::fs::remove_file(path).await {
                    warn!(path = %path.display(), error = %e, "failed to delete debrid download file");
                }
                if let Some(parent) = path.parent() {
                    let _ = tokio::fs::remove_dir(parent).await;
                }
            }
        }
        Ok(())
    }

    async fn test_connection(
        &self,
        _http_client: &reqwest::Client,
        client: &DownloadClientConfig,
    ) -> TestConnectionResult {
        let provider = match Self::provider_id(client) {
            Ok(id) => self.resolve_provider_by_id(id).await,
            Err(e) => Err(e),
        };
        let (provider, api_key) = match provider {
            Ok(pair) => pair,
            Err(e) => {
                return TestConnectionResult {
                    success: false,
                    message: format!("Failed to resolve debrid provider: {e}"),
                };
            }
        };

        match provider
            .get_account_status(&self.http_client, &api_key)
            .await
        {
            Ok(status) => {
                let premium_str = if status.premium { "premium" } else { "free" };
                let user = status.username.as_deref().unwrap_or("unknown");
                TestConnectionResult {
                    success: true,
                    message: format!(
                        "Connected to {} ({}, {})",
                        status.provider, user, premium_str
                    ),
                }
            }
            Err(e) => TestConnectionResult {
                success: false,
                message: format!("Failed to connect: {e}"),
            },
        }
    }
}

/// Background task: unrestrict and download all torrent links.
/// Updates progress in DOWNLOAD_STATE as files complete.
async fn download_all_links(
    http: &reqwest::Client,
    provider: &dyn crate::debrid::DebridProvider,
    api_key: &str,
    links: &[String],
    download_dir: &str,
    info_hash: &str,
    ext_id_key: &str,
) -> Result<Vec<ScannedFile>, DownloadClientError> {
    let download_base = Path::new(download_dir).join(info_hash);
    let mut files = Vec::new();
    let mut completed_bytes: u64 = 0;

    // Estimate total from link count (refined per-file during download)
    let total_estimate = DOWNLOAD_STATE
        .get(ext_id_key)
        .map(|s| {
            if let DebridDownloadProgress::Downloading { total_bytes, .. } = s.value() {
                *total_bytes
            } else {
                0
            }
        })
        .unwrap_or(0);

    for link in links {
        let unrestricted = match provider.unrestrict_link(http, api_key, link).await {
            Ok(u) => u,
            Err(e) => {
                warn!(error = %e, link = %link, "unrestrict_link failed, skipping");
                continue;
            }
        };

        let dest = download_base.join(&unrestricted.filename);

        // Skip if already downloaded (resume after restart)
        if dest.exists()
            && let Ok(meta) = tokio::fs::metadata(&dest).await
            && meta.len() == unrestricted.filesize as u64
        {
            info!(filename = %unrestricted.filename, "Already downloaded, skipping");
            completed_bytes += unrestricted.filesize as u64;
            files.push(scanned_file_from_path(
                &dest,
                &unrestricted.filename,
                unrestricted.filesize,
            ));
            continue;
        }

        info!(
            filename = %unrestricted.filename,
            size = unrestricted.filesize,
            "Downloading from debrid"
        );

        match stream_download_file(http, &unrestricted.url, &dest).await {
            Ok(bytes) => {
                info!(filename = %unrestricted.filename, bytes, "Download complete");
                completed_bytes += bytes;
                files.push(scanned_file_from_path(
                    &dest,
                    &unrestricted.filename,
                    unrestricted.filesize,
                ));

                // Update progress
                DOWNLOAD_STATE.insert(
                    ext_id_key.to_string(),
                    DebridDownloadProgress::Downloading {
                        completed_bytes,
                        total_bytes: total_estimate.max(completed_bytes),
                    },
                );
            }
            Err(e) => {
                warn!(
                    filename = %unrestricted.filename,
                    error = %e,
                    "Download failed, skipping file"
                );
            }
        }
    }

    Ok(files)
}

/// Stream-download a file from URL to local path (standalone, for background tasks).
async fn stream_download_file(
    http: &reqwest::Client,
    url: &str,
    dest: &Path,
) -> Result<u64, DownloadClientError> {
    let resp = http
        .get(url)
        .send()
        .await
        .map_err(DownloadClientError::Request)?;

    if !resp.status().is_success() {
        return Err(DownloadClientError::Message(format!(
            "HTTP {} downloading from debrid",
            resp.status()
        )));
    }

    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| DownloadClientError::Message(format!("mkdir failed: {e}")))?;
    }

    let mut stream = resp.bytes_stream();
    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| DownloadClientError::Message(format!("create file failed: {e}")))?;

    let mut total = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(DownloadClientError::Request)?;
        file.write_all(&chunk)
            .await
            .map_err(|e| DownloadClientError::Message(format!("write file failed: {e}")))?;
        total += chunk.len() as u64;
    }

    file.flush()
        .await
        .map_err(|e| DownloadClientError::Message(format!("flush file failed: {e}")))?;

    Ok(total)
}

/// Build a ScannedFile from a downloaded file path.
fn scanned_file_from_path(path: &Path, filename: &str, size: i64) -> ScannedFile {
    let extension = get_extension(filename);
    let relative_path = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| filename.to_string());

    ScannedFile {
        id: Uuid::now_v7(),
        absolute_path: path.to_string_lossy().to_string(),
        relative_path,
        size,
        extension,
        split_rar_volumes: None,
        distributed_rar_sources: None,
        distributed_numeric_sources: None,
        numeric_split_type: None,
        score: None,
        classification: None,
        file_metadata: None,
        mime_type: None,
    }
}
