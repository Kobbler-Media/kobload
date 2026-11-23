mod config;
mod health;
mod import;
mod jobs;
mod secret;
mod token;

pub use config::{EngineConfigResponse, IpPreference, UsenetServer};
pub use health::HealthSample;
pub use import::{ImportMode, ImportRequest, ImportResponse, OutputInfo};
pub use jobs::{JobFailure, JobRequest, JobState, JobStatus};
pub use secret::{MIN_SECRET_BYTES, SecretError, check_secrets, keys_match};
pub use token::{Purpose, StreamClaims, TokenError, sign, verify};

#[cfg(test)]
mod debug_tests {
    use super::*;

    #[test]
    fn debug_output_never_carries_a_password_or_an_nzb() {
        let import = ImportRequest {
            cache_key: "idx:guid".to_owned(),
            name: "Release".to_owned(),
            mode: ImportMode::Stream,
            password: Some("archive-secret".to_owned()),
            nzb_base64: Some("bnpiLWJvZHk=".to_owned()),
        };
        let job = JobRequest {
            id: "job".to_owned(),
            name: "Release".to_owned(),
            category: "tv".to_owned(),
            nzb_base64: "bnpiLWJvZHk=".to_owned(),
            password: Some("archive-secret".to_owned()),
        };
        for shown in [format!("{import:?}"), format!("{job:?}")] {
            assert!(shown.contains("Release"), "{shown}");
            assert!(!shown.contains("archive-secret"), "{shown}");
            assert!(!shown.contains("bnpiLWJvZHk"), "{shown}");
        }
    }
}
