//! The contract between kobload-api and the usenet engine (`kobload-usenet`): request and
//! response bodies for the engine's control API, the server list the engine pulls from
//! kobload, and the HMAC-signed tokens that let kobbler read stream bytes straight from the
//! engine without either side sharing provider credentials or indexer keys.

mod config;
mod import;
mod jobs;
mod secret;
mod token;

pub use config::{EngineConfigResponse, IpPreference, UsenetServer};
pub use import::{ImportMode, ImportRequest, ImportResponse, OutputInfo};
pub use jobs::{JobFailure, JobRequest, JobState, JobStatus};
pub use secret::{MIN_SECRET_BYTES, SecretError, check_secrets, keys_match};
pub use token::{Purpose, StreamClaims, TokenError, sign, verify};
