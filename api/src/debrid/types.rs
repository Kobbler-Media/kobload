use crate::parsing::file::VIDEO_EXTENSIONS;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Supported debrid provider types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DebridProviderType {
    RealDebrid,
    AllDebrid,
    Premiumize,
    TorBox,
}

impl DebridProviderType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RealDebrid => "real_debrid",
            Self::AllDebrid => "alldebrid",
            Self::Premiumize => "premiumize",
            Self::TorBox => "torbox",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "real_debrid" => Some(Self::RealDebrid),
            "alldebrid" => Some(Self::AllDebrid),
            "premiumize" => Some(Self::Premiumize),
            "torbox" => Some(Self::TorBox),
            _ => None,
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::RealDebrid => "Real-Debrid",
            Self::AllDebrid => "AllDebrid",
            Self::Premiumize => "Premiumize",
            Self::TorBox => "TorBox",
        }
    }

    /// Short prefix used in provider-prefixed torrent IDs (e.g. "rd_abc123").
    pub fn prefix(&self) -> &'static str {
        match self {
            Self::RealDebrid => "rd",
            Self::AllDebrid => "ad",
            Self::Premiumize => "pm",
            Self::TorBox => "tb",
        }
    }

    /// Create a provider-prefixed torrent ID like "rd_abc123".
    pub fn prefixed_id(&self, id: &str) -> String {
        format!("{}_{}", self.prefix(), id)
    }

    /// Parse a provider-prefixed ID back into (provider_type, raw_id).
    pub fn from_prefixed_id(prefixed: &str) -> Option<(Self, &str)> {
        let (prefix, raw) = prefixed.split_once('_')?;
        let provider = match prefix {
            "rd" => Self::RealDebrid,
            "ad" => Self::AllDebrid,
            "pm" => Self::Premiumize,
            "tb" => Self::TorBox,
            _ => return None,
        };
        Some((provider, raw))
    }
}

impl fmt::Display for DebridProviderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

/// A file within a debrid-cached torrent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebridFile {
    /// Provider-specific file ID (used for file selection).
    pub id: i32,
    /// File path within the torrent.
    pub path: String,
    /// File size in bytes.
    pub size: i64,
    /// Whether the torrent serves this file. Only Real-Debrid selects; elsewhere every
    /// file of a torrent is served.
    #[serde(default)]
    pub selected: bool,
}

impl DebridFile {
    /// Whether this file has a recognized video extension.
    fn is_video(&self) -> bool {
        let basename = self.path.rsplit('/').next().unwrap_or(&self.path);
        let ext = basename.rsplit('.').next().unwrap_or("");
        VIDEO_EXTENSIONS.contains(&ext.to_lowercase().as_str())
    }

    fn is_sample(&self) -> bool {
        self.path
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .any(|t| t == "sample")
    }

    /// The video file a request stands for: a basename match, else the provider file
    /// `id`, else the largest video.
    pub fn wanted<'a>(files: &'a [Self], name: Option<&str>, id: Option<i32>) -> Option<&'a Self> {
        let videos = || files.iter().filter(|f| f.is_video());
        let basename = |f: &Self| f.path.rsplit('/').next().unwrap_or(&f.path).to_lowercase();
        name.filter(|n| !n.is_empty())
            .map(|n| n.rsplit('/').next().unwrap_or(n).to_lowercase())
            .and_then(|n| videos().find(|f| basename(f) == n))
            .or_else(|| id.and_then(|id| videos().find(|f| f.id == id)))
            .or_else(|| videos().max_by_key(|f| f.size))
    }

    pub fn resolve_select_ids(files: &[Self], wanted_name: Option<&str>) -> Vec<i32> {
        Self::wanted(files, wanted_name, None)
            .map(|f| vec![f.id])
            .unwrap_or_default()
    }

    /// Every episode of a pack, so the next ones play from the same torrent.
    pub fn pack_select_ids(files: &[Self]) -> Vec<i32> {
        files
            .iter()
            .filter(|f| f.is_video() && !f.is_sample())
            .map(|f| f.id)
            .collect()
    }
}

/// Result of checking instant availability for a single hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedAvailability {
    /// The info hash that was checked.
    pub hash: String,
    /// Whether the torrent is cached on this provider.
    pub available: bool,
    /// Available file groupings (multi-file torrents may have multiple valid selections).
    /// Each inner Vec is one valid file selection.
    pub file_variants: Vec<Vec<DebridFile>>,
}

/// Status of a torrent on a debrid provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum DebridTorrentStatus {
    /// Torrent is fully cached, files available instantly.
    Cached,
    /// Torrent is being downloaded/cached by the provider.
    Downloading {
        /// Download progress 0.0 - 1.0.
        progress: f32,
    },
    /// Provider has torrent metadata but needs file selection (RD-specific).
    WaitingFileSelection,
    /// Files selected and download links are ready.
    Ready,
    /// The provider refuses to serve this torrent (RD "virus" status /
    /// error_code 35). Permanent for this hash — do not retry.
    Blocked,
    /// Something went wrong.
    Error { message: String },
}

/// A torrent that has been submitted to a debrid provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebridTorrent {
    /// Provider-specific torrent ID.
    pub id: String,
    /// Current status.
    pub status: DebridTorrentStatus,
    /// Files in the torrent.
    pub files: Vec<DebridFile>,
    /// Download links (available when status is Ready).
    pub links: Vec<String>,
}

impl DebridTorrent {
    pub fn wanted_link(&self, name: Option<&str>, id: Option<i32>) -> Option<&str> {
        match DebridFile::wanted(&self.files, name, id) {
            Some(f) => self.link_for(f.id),
            None => self.links.first().map(String::as_str),
        }
    }

    /// Whether the file a request stands for is served without selecting again, which
    /// Real-Debrid allows only once per torrent.
    pub fn serves(&self, name: Option<&str>, id: Option<i32>) -> bool {
        matches!(self.status, DebridTorrentStatus::WaitingFileSelection)
            || DebridFile::wanted(&self.files, name, id).is_none_or(|f| f.selected)
    }

    /// Real-Debrid lists one link per selected file, in file order. A provider whose links
    /// do not line up with its files gets its first link.
    fn link_for(&self, file_id: i32) -> Option<&str> {
        let mut selected: Vec<i32> = self
            .files
            .iter()
            .filter(|f| f.selected)
            .map(|f| f.id)
            .collect();
        selected.sort_unstable();
        let aligned = selected.len() == self.links.len();
        selected
            .iter()
            .position(|id| *id == file_id)
            .filter(|_| aligned)
            .map_or(self.links.first(), |i| self.links.get(i))
            .map(String::as_str)
    }
}

/// A direct, unrestricted download/stream URL from a debrid provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnrestrictedLink {
    /// Direct HTTP URL for download/streaming.
    pub url: String,
    /// Filename.
    pub filename: String,
    /// File size in bytes.
    pub filesize: i64,
    /// MIME type if known.
    pub mime_type: Option<String>,
    /// When this link expires (provider-specific).
    pub expires_at: Option<time::OffsetDateTime>,
}

/// A usenet download on a debrid provider (TorBox only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsenetDownload {
    /// Provider-specific download ID.
    pub id: String,
    /// Current status.
    pub status: DebridTorrentStatus,
    /// Download links when ready.
    pub links: Vec<String>,
}

/// Account status information from a debrid provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebridAccountStatus {
    /// Provider display name.
    pub provider: String,
    /// Username or email.
    pub username: Option<String>,
    /// Account expiration date.
    pub expiration: Option<String>,
    /// Whether the account is currently premium/active.
    pub premium: bool,
    /// Remaining points/traffic if applicable.
    pub remaining_traffic: Option<String>,
}

/// A torrent already present on the provider account.
#[derive(Debug, Clone)]
pub struct ExistingTorrent {
    /// Provider-specific torrent ID.
    pub id: String,
    /// Current status.
    pub status: DebridTorrentStatus,
    /// Whether download links are already available.
    pub has_links: bool,
}

/// Active torrent slot usage on a provider account.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ActiveSlots {
    /// Torrents currently active (downloading / awaiting selection).
    pub current: u32,
    /// Maximum concurrent active torrents this account allows.
    pub limit: u32,
}

impl ActiveSlots {
    /// Free slots remaining.
    pub fn available(&self) -> u32 {
        self.limit.saturating_sub(self.current)
    }
}

/// Errors from debrid provider operations.
#[derive(Debug)]
pub enum DebridError {
    /// HTTP request failed.
    Request(reqwest::Error),
    /// Provider returned an error status code.
    Status(reqwest::StatusCode, String),
    /// Provider returned an unexpected response format.
    Parse(String),
    /// Operation not supported by this provider (e.g., usenet on non-TorBox).
    UnsupportedOperation,
    /// Authentication failed (bad API key).
    AuthFailed,
    /// Rate limited by the provider.
    RateLimited {
        retry_after: Option<std::time::Duration>,
    },
    /// Torrent not found / hash unknown.
    NotFound,
    /// Provider refuses to serve this content.
    ///
    /// Real-Debrid error_code 35 ("Infringing file"). Since May 2026 this is
    /// applied heuristically to filenames matching common release tags
    /// (WEB-DL, AMZN, YTS, RARBG, ...) rather than being a genuine per-file
    /// takedown, so it is durable for a given hash but says nothing about
    /// whether the content is actually cached.
    Blocked,
    /// The account has too many active torrents right now.
    ///
    /// Real-Debrid error_code 33 ("Torrent already active"). Transient — the
    /// hash itself is fine, we just have no free slot.
    TooManyActive,
    /// Provider-side fair-usage throttle (Real-Debrid error_code 36).
    FairUsage,
    /// Generic error with message.
    Other(String),
}

impl DebridError {
    /// Whether this error is transient and the operation could be retried.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Request(_) | Self::RateLimited { .. } | Self::TooManyActive | Self::FairUsage => {
                true
            }
            Self::Status(status, _) => status.is_server_error(),
            _ => false,
        }
    }

    /// Whether this error means the hash is permanently unusable on this
    /// provider, so it should be negatively cached for a long time rather
    /// than re-probed on the next search.
    pub fn is_permanent_for_hash(&self) -> bool {
        matches!(self, Self::Blocked)
    }

    /// Whether this error says nothing about the hash itself and so must NOT
    /// be recorded as a negative availability result.
    pub fn is_inconclusive(&self) -> bool {
        matches!(
            self,
            Self::Request(_)
                | Self::RateLimited { .. }
                | Self::TooManyActive
                | Self::FairUsage
                | Self::AuthFailed
        )
    }
}

impl fmt::Display for DebridError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(e) => write!(f, "HTTP request failed: {e}"),
            Self::Status(code, msg) => write!(f, "Provider error {code}: {msg}"),
            Self::Parse(msg) => write!(f, "Parse error: {msg}"),
            Self::UnsupportedOperation => write!(f, "Operation not supported by this provider"),
            Self::AuthFailed => write!(f, "Authentication failed"),
            Self::RateLimited { retry_after } => {
                write!(f, "Rate limited")?;
                if let Some(d) = retry_after {
                    write!(f, " (retry after {d:?})")?;
                }
                Ok(())
            }
            Self::NotFound => write!(f, "Not found"),
            Self::Blocked => write!(f, "Provider refused this file (infringing/filtered)"),
            Self::TooManyActive => write!(f, "Too many active torrents on the provider"),
            Self::FairUsage => write!(f, "Provider fair usage limit reached"),
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for DebridError {}

impl From<reqwest::Error> for DebridError {
    fn from(e: reqwest::Error) -> Self {
        Self::Request(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(id: i32, path: &str, size: i64) -> DebridFile {
        DebridFile {
            id,
            path: path.to_string(),
            size,
            selected: false,
        }
    }

    #[test]
    fn selects_largest_video_file() {
        let files = vec![
            file(1, "movie/setup.exe", 2_000_000_000),
            file(2, "movie/The.Movie.2026.1080p.mkv", 1_500_000_000),
            file(3, "movie/sample.mkv", 50_000_000),
        ];
        assert_eq!(DebridFile::resolve_select_ids(&files, None), vec![2]);
    }

    #[test]
    fn rejects_all_non_video_files() {
        let files = vec![
            file(1, "fake/setup.exe", 2_000_000_000),
            file(2, "fake/readme.txt", 1_000),
            file(3, "fake/install.msi", 500_000_000),
        ];
        assert_eq!(
            DebridFile::resolve_select_ids(&files, None),
            Vec::<i32>::new()
        );
    }

    #[test]
    fn name_match_skips_non_video() {
        let files = vec![
            file(1, "The.Movie.2026.exe", 2_000_000_000),
            file(2, "The.Movie.2026.mkv", 1_500_000_000),
        ];
        // Name matches the .exe but it should be skipped, fallback to .mkv
        let result = DebridFile::resolve_select_ids(&files, Some("The.Movie.2026.exe"));
        assert_eq!(result, vec![2]);
    }

    #[test]
    fn name_match_on_video_works() {
        let files = vec![
            file(1, "season1/S01E01.mkv", 500_000_000),
            file(2, "season1/S01E02.mkv", 800_000_000),
        ];
        let result = DebridFile::resolve_select_ids(&files, Some("S01E01.mkv"));
        assert_eq!(result, vec![1]);
    }

    #[test]
    fn empty_files_returns_empty() {
        assert_eq!(DebridFile::resolve_select_ids(&[], None), Vec::<i32>::new());
    }

    #[test]
    fn nested_path_extracts_basename() {
        let files = vec![
            file(1, "torrent/subs/english.srt", 50_000),
            file(2, "torrent/video/movie.mp4", 1_000_000_000),
        ];
        assert_eq!(DebridFile::resolve_select_ids(&files, None), vec![2]);
    }
}
