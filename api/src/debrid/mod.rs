pub mod alldebrid;
pub mod magnet;
pub mod premiumize;
pub mod provider;
pub mod rd_names;
pub mod real_debrid;
mod registry;
pub mod throttle;
pub mod torbox;
pub mod types;

pub use provider::DebridProvider;
pub use registry::{DebridProviderResolver, DefaultDebridProviderResolver};
pub use throttle::{Lane, Throttled};
pub use types::{
    AddPauses, CachedAvailability, DebridAccountStatus, DebridError, DebridFile,
    DebridProviderType, DebridTorrent, DebridTorrentStatus, ExistingTorrent, UnrestrictedLink,
    UsenetDownload,
};
