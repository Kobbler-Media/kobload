//! External API source clients for the torrent hash index.
//!
//! Each sub-module implements a client for a specific external API that provides
//! torrent hash metadata. All clients return `Vec<HashIndexEntry>` and handle
//! errors gracefully (returning empty results on failure).

pub mod eztv;
pub mod torrentio;
pub mod zilean;

pub use eztv::EztvClient;
pub use torrentio::TorrentioClient;
pub use zilean::ZileanClient;
