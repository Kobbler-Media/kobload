mod downloads;
mod indexers;
mod validation;

use axum::{http::StatusCode, response::IntoResponse};
use sqlx::PgPool;
use tracing::warn;

use crate::repositories::config as config_repo;
use crate::types::ProwlarrConfig;

pub use downloads::*;
pub use indexers::*;
pub use validation::*;

pub async fn load_prowlarr_config(
    pool: &PgPool,
) -> Result<ProwlarrConfig, axum::response::Response> {
    match config_repo::get_prowlarr_config(pool).await {
        Ok(Some(config)) => Ok(ProwlarrConfig {
            base_url: config.base_url,
            api_key: config.api_key,
        }),
        Ok(None) => Ok(ProwlarrConfig {
            base_url: None,
            api_key: None,
        }),
        Err(err) => {
            warn!(error = %err, "Failed to load prowlarr config");
            Err((StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_email() {
        assert_eq!(
            normalize_email("  user@example.com ").expect("email"),
            "user@example.com"
        );
        assert!(normalize_email("   ").is_err());
    }

    #[test]
    fn test_validate_password() {
        assert!(validate_password("longpassword", false).is_ok());
        assert!(validate_password("12345678", false).is_ok());
        assert!(validate_password("short", false).is_err());
        assert!(validate_password("   ", false).is_err());
        assert!(validate_password("   ", true).is_ok());
    }
}
