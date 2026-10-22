use axum::{Json, Router, extract::State, routing::get};
use serde::Serialize;

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/info", get(api_info))
        .route("/api/v1/nats/info", get(nats_info))
}

#[derive(Serialize)]
struct ApiInfo {
    name: &'static str,
    version: &'static str,
    endpoints: Vec<EndpointGroup>,
}

#[derive(Serialize)]
struct EndpointGroup {
    group: &'static str,
    endpoints: Vec<Endpoint>,
}

#[derive(Serialize)]
struct Endpoint {
    method: &'static str,
    path: &'static str,
    description: &'static str,
    auth: &'static str,
}

// ============================================================================
// NATS info
// ============================================================================

#[derive(Serialize)]
struct NatsInfo {
    connected: bool,
    jobs: Vec<NatsJobInfo>,
    events: Vec<NatsEventInfo>,
    scheduled: Vec<NatsScheduleInfo>,
}

#[derive(Serialize)]
struct NatsJobInfo {
    subject: &'static str,
    description: &'static str,
    max_concurrent: usize,
    http_equivalent: Option<&'static str>,
    /// OpenAPI component name for the input/request schema
    input_schema: &'static str,
    /// OpenAPI component name for the output/response schema (if any)
    #[serde(skip_serializing_if = "Option::is_none")]
    response_schema: Option<&'static str>,
}

#[derive(Serialize)]
struct NatsEventInfo {
    subject: &'static str,
    description: &'static str,
    direction: &'static str,
    /// OpenAPI component name for the event payload schema
    payload_schema: &'static str,
}

#[derive(Serialize)]
struct NatsScheduleInfo {
    name: &'static str,
    subject: &'static str,
    interval_seconds: u64,
    description: &'static str,
}

async fn nats_info(State(state): State<AppState>) -> Json<NatsInfo> {
    Json(NatsInfo {
        connected: state.nats.is_available(),
        jobs: vec![
            NatsJobInfo {
                subject: "search_request",
                description: "Search indexers for releases",
                max_concurrent: 5,
                http_equivalent: Some("POST /api/v1/search"),
                input_schema: "SearchRequest",
                response_schema: Some("SearchResponse"),
            },
            NatsJobInfo {
                subject: "want_request",
                description: "Create want request (search + watch). Publishes response to want.response.{source_system}",
                max_concurrent: 3,
                http_equivalent: Some("POST /api/v1/want"),
                input_schema: "WantRequest",
                response_schema: Some("WantResponse"),
            },
            NatsJobInfo {
                subject: "watch_pattern_delete",
                description: "Delete watch pattern by source_system/source_id",
                max_concurrent: 1,
                http_equivalent: Some("POST /api/v1/watch/bulk-delete"),
                input_schema: "WatchPatternDelete",
                response_schema: None,
            },
            NatsJobInfo {
                subject: "indexer_sync",
                description: "Sync indexer sources from Prowlarr",
                max_concurrent: 1,
                http_equivalent: Some("POST /api/v1/admin/indexer-sources/{id}/check"),
                input_schema: "{}",
                response_schema: None,
            },
        ],
        events: vec![
            NatsEventInfo {
                subject: "download.completed",
                description: "Fired when a download completes or fails",
                direction: "publish",
                payload_schema: "DownloadCompletedEvent",
            },
            NatsEventInfo {
                subject: "download.watch_matched",
                description: "Fired when a watch pattern matches a new release",
                direction: "publish",
                payload_schema: "WatchMatchedEvent",
            },
            NatsEventInfo {
                subject: "want.response.{source_system}",
                description: "Want result published after want_request job completes",
                direction: "publish",
                payload_schema: "WantResponse",
            },
        ],
        scheduled: vec![NatsScheduleInfo {
            name: "indexer-sync",
            subject: "indexer_sync",
            interval_seconds: 3600,
            description: "Sync Prowlarr indexers hourly",
        }],
    })
}

// ============================================================================
// API info
// ============================================================================

async fn api_info() -> Json<ApiInfo> {
    Json(ApiInfo {
        name: "Kobload API",
        version: env!("CARGO_PKG_VERSION"),
        endpoints: vec![
            EndpointGroup {
                group: "Auth",
                endpoints: vec![
                    Endpoint {
                        method: "POST",
                        path: "/api/v1/auth/login",
                        description: "Login with email/password",
                        auth: "none",
                    },
                    Endpoint {
                        method: "POST",
                        path: "/api/v1/auth/logout",
                        description: "Logout current session",
                        auth: "session",
                    },
                    Endpoint {
                        method: "GET",
                        path: "/api/v1/auth/me",
                        description: "Current user info",
                        auth: "session",
                    },
                ],
            },
            EndpointGroup {
                group: "Search & Download",
                endpoints: vec![
                    Endpoint {
                        method: "POST",
                        path: "/api/v1/search",
                        description: "Search indexers",
                        auth: "api_key",
                    },
                    Endpoint {
                        method: "POST",
                        path: "/api/v1/search-download",
                        description: "Search and download best match",
                        auth: "api_key",
                    },
                    Endpoint {
                        method: "POST",
                        path: "/api/v1/want",
                        description: "Create a want request (search + watch)",
                        auth: "api_key",
                    },
                    Endpoint {
                        method: "POST",
                        path: "/api/v1/want/bulk",
                        description: "Bulk create want requests",
                        auth: "api_key",
                    },
                    Endpoint {
                        method: "POST",
                        path: "/api/v1/want/{source_system}/{source_id}/confirm",
                        description: "Confirm want fulfilled",
                        auth: "api_key",
                    },
                ],
            },
            EndpointGroup {
                group: "Downloads",
                endpoints: vec![
                    Endpoint {
                        method: "GET",
                        path: "/api/v1/downloads",
                        description: "List downloads",
                        auth: "any",
                    },
                    Endpoint {
                        method: "POST",
                        path: "/api/v1/downloads",
                        description: "Create download",
                        auth: "any",
                    },
                    Endpoint {
                        method: "GET",
                        path: "/api/v1/downloads/{id}",
                        description: "Get download details",
                        auth: "any",
                    },
                    Endpoint {
                        method: "DELETE",
                        path: "/api/v1/downloads/{id}",
                        description: "Delete download",
                        auth: "any",
                    },
                    Endpoint {
                        method: "GET",
                        path: "/api/v1/downloads/{id}/file",
                        description: "Stream downloaded file",
                        auth: "any",
                    },
                    Endpoint {
                        method: "GET",
                        path: "/api/v1/downloads/{id}/files",
                        description: "List files in download",
                        auth: "any",
                    },
                    Endpoint {
                        method: "PATCH",
                        path: "/api/v1/downloads/{id}/retry",
                        description: "Retry failed download callback",
                        auth: "any",
                    },
                ],
            },
            EndpointGroup {
                group: "Watch",
                endpoints: vec![
                    Endpoint {
                        method: "GET",
                        path: "/api/v1/admin/watch/patterns",
                        description: "List watch patterns",
                        auth: "admin",
                    },
                    Endpoint {
                        method: "POST",
                        path: "/api/v1/admin/watch/patterns",
                        description: "Create watch pattern",
                        auth: "admin",
                    },
                    Endpoint {
                        method: "DELETE",
                        path: "/api/v1/admin/watch/patterns/{id}",
                        description: "Delete watch pattern",
                        auth: "admin",
                    },
                    Endpoint {
                        method: "POST",
                        path: "/api/v1/admin/watch/{id}/test",
                        description: "Test pattern against RSS cache",
                        auth: "admin",
                    },
                    Endpoint {
                        method: "GET",
                        path: "/api/v1/admin/watch/matches",
                        description: "List watch matches",
                        auth: "admin",
                    },
                ],
            },
            EndpointGroup {
                group: "Settings",
                endpoints: vec![
                    Endpoint {
                        method: "GET",
                        path: "/api/v1/admin/indexer-sources",
                        description: "List indexer sources",
                        auth: "admin",
                    },
                    Endpoint {
                        method: "POST",
                        path: "/api/v1/admin/indexer-sources",
                        description: "Create indexer source",
                        auth: "admin",
                    },
                    Endpoint {
                        method: "GET",
                        path: "/api/v1/admin/download-clients",
                        description: "List download clients",
                        auth: "admin",
                    },
                    Endpoint {
                        method: "GET",
                        path: "/api/v1/admin/profiles",
                        description: "List profiles",
                        auth: "admin",
                    },
                    Endpoint {
                        method: "GET",
                        path: "/api/v1/admin/quality-profiles",
                        description: "List quality profiles",
                        auth: "admin",
                    },
                    Endpoint {
                        method: "GET",
                        path: "/api/v1/admin/users",
                        description: "List users",
                        auth: "admin",
                    },
                    Endpoint {
                        method: "GET",
                        path: "/api/v1/admin/api-keys",
                        description: "List API keys",
                        auth: "admin",
                    },
                ],
            },
            EndpointGroup {
                group: "System",
                endpoints: vec![
                    Endpoint {
                        method: "GET",
                        path: "/health",
                        description: "Health check",
                        auth: "none",
                    },
                    Endpoint {
                        method: "GET",
                        path: "/api/v1/info",
                        description: "API info and endpoint listing",
                        auth: "none",
                    },
                    Endpoint {
                        method: "GET",
                        path: "/api/v1/admin/system/health",
                        description: "System health dashboard",
                        auth: "admin",
                    },
                    Endpoint {
                        method: "GET",
                        path: "/api/v1/admin/webhooks",
                        description: "List status webhooks",
                        auth: "admin",
                    },
                    Endpoint {
                        method: "POST",
                        path: "/api/v1/admin/webhooks",
                        description: "Create status webhook",
                        auth: "admin",
                    },
                    Endpoint {
                        method: "PUT",
                        path: "/api/v1/admin/webhooks/{id}",
                        description: "Update webhook",
                        auth: "admin",
                    },
                    Endpoint {
                        method: "DELETE",
                        path: "/api/v1/admin/webhooks/{id}",
                        description: "Delete webhook",
                        auth: "admin",
                    },
                ],
            },
        ],
    })
}
