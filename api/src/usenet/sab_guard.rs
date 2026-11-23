use sqlx::PgPool;
use tracing::warn;
use uuid::Uuid;

use super::caps::sab_violation;
use super::sab_share::{candidate_servers, queue_working, sab_servers_except};
use crate::repositories::{RepositoryError, usenet_servers};
use crate::types::DownloadClientConfig;

// The engine yields to SAB on a shared account, down to no connections at all, so a SAB
// that would push an engine server out is refused before it is saved enabled.
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

// An unreachable SAB is not refused: an admin must be able to remove a dead client.
pub async fn sab_delete_refusal(
    http: &reqwest::Client,
    sab: &DownloadClientConfig,
) -> Option<String> {
    match queue_working(http, sab).await {
        Ok(true) => Some(format!(
            "{} still has an active download queue and holds its usenet connections. Let the queue finish, or pause or clear it in SABnzbd, then delete this client.",
            sab.name
        )),
        Ok(false) => None,
        Err(reason) => {
            warn!(client = %sab.name, reason, "Could not read SABnzbd's queue; allowing its deletion");
            None
        }
    }
}
