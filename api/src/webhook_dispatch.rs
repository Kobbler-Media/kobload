//! Dispatches download status change events to registered webhooks.

use serde::Serialize;
use sqlx::PgPool;
use time::OffsetDateTime;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::repositories::webhooks;

#[derive(Debug, Clone, Serialize)]
pub struct WebhookPayload {
    pub event: String,
    pub download_id: Uuid,
    pub status: String,
    pub title: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
}

/// Fire webhooks for a download status change event.
///
/// This is fire-and-forget — failures are logged but don't block the caller.
pub async fn dispatch_download_event(
    pool: &PgPool,
    http_client: &reqwest::Client,
    event: &str,
    download_id: Uuid,
    status: &str,
    title: Option<&str>,
) {
    let hooks = match webhooks::list_enabled_for_event(pool, event).await {
        Ok(h) => h,
        Err(e) => {
            warn!(error = %e, "Failed to load webhooks for dispatch");
            return;
        }
    };

    if hooks.is_empty() {
        return;
    }

    let payload = WebhookPayload {
        event: event.to_string(),
        download_id,
        status: status.to_string(),
        title: title.map(|s| s.to_string()),
        timestamp: OffsetDateTime::now_utc(),
    };

    for hook in hooks {
        let client = http_client.clone();
        let payload = payload.clone();
        let url = hook.url.clone();
        let secret = hook.secret.clone();

        tokio::spawn(async move {
            let mut req = client.post(&url).json(&payload);
            if let Some(ref s) = secret {
                req = req.header("X-Webhook-Secret", s);
            }

            match req.send().await {
                Ok(resp) if resp.status().is_success() => {
                    debug!(url = %url, event = %payload.event, "Webhook delivered");
                }
                Ok(resp) => {
                    warn!(url = %url, status = %resp.status(), "Webhook delivery got non-success response");
                }
                Err(e) => {
                    warn!(url = %url, error = %e, "Webhook delivery failed");
                }
            }
        });
    }
}
