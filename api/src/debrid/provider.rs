use async_trait::async_trait;

use super::types::{
    ActiveSlots, CachedAvailability, DebridAccountStatus, DebridError, DebridProviderType,
    DebridTorrent, ExistingTorrent, UnrestrictedLink, UsenetDownload,
};

/// Trait implemented by each debrid provider (Real-Debrid, AllDebrid, Premiumize, TorBox).
///
/// All methods take an `http_client` and `api_key` rather than storing them,
/// following the download client pattern where handlers are stateless.
#[async_trait]
pub trait DebridProvider: Send + Sync {
    /// Provider type identifier.
    fn provider_type(&self) -> DebridProviderType;

    /// Human-readable provider name.
    fn display_name(&self) -> &str {
        self.provider_type().display_name()
    }

    /// Whether this provider supports usenet downloads (only TorBox).
    fn supports_usenet(&self) -> bool {
        false
    }

    /// Maximum number of hashes per availability check request.
    fn max_hashes_per_batch(&self) -> usize {
        100
    }

    // ── Torrent operations ──────────────────────────────────────────────

    /// Check instant availability for a batch of info hashes.
    /// Returns only entries for hashes that are cached.
    async fn check_availability(
        &self,
        http_client: &reqwest::Client,
        api_key: &str,
        hashes: &[&str],
    ) -> Result<Vec<CachedAvailability>, DebridError>;

    /// Submit a magnet link for caching. Returns a provider-specific torrent ID.
    async fn add_magnet(
        &self,
        http_client: &reqwest::Client,
        api_key: &str,
        magnet: &str,
    ) -> Result<String, DebridError>;

    /// Select which files to download from a submitted torrent.
    async fn select_files(
        &self,
        http_client: &reqwest::Client,
        api_key: &str,
        torrent_id: &str,
        file_ids: &[i32],
    ) -> Result<(), DebridError>;

    /// Get the current status and download links for a submitted torrent.
    async fn get_torrent(
        &self,
        http_client: &reqwest::Client,
        api_key: &str,
        torrent_id: &str,
    ) -> Result<DebridTorrent, DebridError>;

    /// Convert a provider download link to a direct, unrestricted stream URL.
    async fn unrestrict_link(
        &self,
        http_client: &reqwest::Client,
        api_key: &str,
        link: &str,
    ) -> Result<UnrestrictedLink, DebridError>;

    /// Torrents already on the account, keyed by lowercase info hash, every copy of a hash
    /// best-first: copies select different files, and RD allows selecting only once.
    ///
    /// Real-Debrid does not deduplicate: re-adding a hash it already holds
    /// usually creates a second entry rather than returning the existing one
    /// or erroring. Measured on a live account, 21 of 100 entries were
    /// redundant copies. Callers should look here before `add_magnet` so a
    /// repeated search reuses what is already cached instead of accumulating
    /// duplicates and burning active-torrent slots.
    ///
    /// Returns an empty map for providers that do not expose a listing.
    async fn find_existing_torrents(
        &self,
        _http_client: &reqwest::Client,
        _api_key: &str,
    ) -> Result<std::collections::HashMap<String, Vec<ExistingTorrent>>, DebridError> {
        Ok(std::collections::HashMap::new())
    }

    /// Remove a torrent from the provider account.
    ///
    /// Cache warming submits magnets purely to discover whether they are
    /// cached; the ones we don't end up streaming must be removed or they
    /// accumulate and consume the account's active-torrent slots (Real-Debrid
    /// then rejects new additions with error_code 33).
    ///
    /// Default is a no-op for providers where submitting is free of slot cost.
    async fn delete_torrent(
        &self,
        _http_client: &reqwest::Client,
        _api_key: &str,
        _torrent_id: &str,
    ) -> Result<(), DebridError> {
        Ok(())
    }

    /// Number of currently active torrents and the account maximum, if the
    /// provider exposes it. Used to avoid warming into a full slot pool.
    async fn active_slots(
        &self,
        _http_client: &reqwest::Client,
        _api_key: &str,
    ) -> Result<Option<ActiveSlots>, DebridError> {
        Ok(None)
    }

    // ── Usenet operations (TorBox only, default = unsupported) ──────────

    /// Check if usenet content is cached. Returns availability per ID.
    async fn check_usenet_cached(
        &self,
        _http_client: &reqwest::Client,
        _api_key: &str,
        _ids: &[&str],
    ) -> Result<Vec<bool>, DebridError> {
        Err(DebridError::UnsupportedOperation)
    }

    /// Submit an NZB URL for download. Returns a provider-specific download ID.
    async fn add_nzb(
        &self,
        _http_client: &reqwest::Client,
        _api_key: &str,
        _nzb_url: &str,
    ) -> Result<String, DebridError> {
        Err(DebridError::UnsupportedOperation)
    }

    /// Get the current status and download links for a usenet download.
    async fn get_usenet_download(
        &self,
        _http_client: &reqwest::Client,
        _api_key: &str,
        _id: &str,
    ) -> Result<UsenetDownload, DebridError> {
        Err(DebridError::UnsupportedOperation)
    }

    // ── Account operations ──────────────────────────────────────────────

    /// Test the connection and return account status.
    /// Used for test-before-save and status display.
    async fn get_account_status(
        &self,
        http_client: &reqwest::Client,
        api_key: &str,
    ) -> Result<DebridAccountStatus, DebridError>;
}
