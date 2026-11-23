use sqlx::PgPool;
use uuid::Uuid;

use super::caps::sab_violation;
use super::sab_share::{candidate_servers, sab_servers_except};
use crate::repositories::{RepositoryError, usenet_servers};
use crate::types::DownloadClientConfig;

// The engine yields to SAB on a shared account, but only down to one connection per
// server, so a SAB that alone fills the cap is refused before it is saved enabled.
pub async fn sab_client_refusal(
    pool: &PgPool,
    http: &reqwest::Client,
    candidate: &DownloadClientConfig,
) -> Result<Option<String>, RepositoryError> {
    if candidate.client_type != "sabnzbd" || !candidate.enabled {
        return Ok(None);
    }
    let rows = usenet_servers::list(pool).await?;
    if !rows.iter().any(|r| r.enabled) {
        return Ok(None);
    }
    let except = (candidate.id != Uuid::nil()).then_some(candidate.id);
    let mut sab = sab_servers_except(pool, http, except).await;
    sab.extend(candidate_servers(http, candidate).await);
    Ok(sab_violation(&rows, &sab).map(|over| over.describe_sab()))
}
