//! OpenAPI specification and Swagger UI setup.

use axum::Router;
use utoipa::openapi::path::{HttpMethod, OperationBuilder, PathItemBuilder};
use utoipa::openapi::response::ResponseBuilder;
use utoipa::openapi::response::ResponsesBuilder;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::openapi::{self, Components};
use utoipa::{Modify, OpenApi};

use crate::state::AppState;

const API_DESCRIPTION: &str = "\
Generic download orchestration service. Manages search, download, monitoring, \
and file delivery across indexers and download clients.\n\n\
## NATS Interface\n\n\
In addition to HTTP, Kobload exposes a NATS JetStream job interface for async \
inter-service communication. Key subjects:\n\n\
| Subject | Description |\n\
|---------|-------------|\n\
| `search_request` | Async search (equivalent to POST /api/v1/search) |\n\
| `want_request` | Async want (equivalent to POST /api/v1/want), publishes response to `want.response.{source_system}` |\n\
| `watch_pattern_upsert` | Create/update watch pattern by source_system/source_id |\n\
| `watch_pattern_delete` | Delete watch pattern by source_system/source_id |\n\
| `indexer_sync` | Scheduled hourly indexer sync from Prowlarr |\n\n\
**Events** (pub/sub):\n\n\
| Subject | Description |\n\
|---------|-------------|\n\
| `download.completed` | Fired when a download completes or fails |\n\
| `download.watch_matched` | Fired when a watch pattern matches a new release |\n\
| `want.response.{source_system}` | Want result published after want_request job completes |\n\n\
See `nats-jobs` in koblibs for type definitions.";

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Kobload API",
        version = "0.1.0",
        license(name = "Proprietary"),
    ),
    components(schemas(
        nats_jobs::DownloadFileInfo,
        nats_jobs::FileGroupInfo,
        nats_jobs::SearchRequest,
        nats_jobs::SearchResponse,
        nats_jobs::SearchResult,
        nats_jobs::WantRequest,
        nats_jobs::WantResponse,
        nats_jobs::WantStatus,
        nats_jobs::WatchPatternUpsert,
        nats_jobs::WatchPatternDelete,
        nats_jobs::WatchMatchedEvent,
        nats_jobs::DownloadCompletedEvent,
        nats_jobs::MediaWant,
        nats_jobs::MediaKind,
        nats_jobs::BookIdentifiers,
        nats_jobs::TvIdentifiers,
        nats_jobs::MovieIdentifiers,
        nats_jobs::QualityOverrides,
        nats_jobs::MatchTarget,
        nats_jobs::ProcessedReleaseDto,
        nats_jobs::SearchAndDownloadRequest,
        nats_jobs::SearchAndDownloadResponse,
    )),
    modifiers(&SecurityAddon, &DescriptionAddon),
    tags(
        (name = "auth", description = "Authentication"),
        (name = "downloads", description = "Download management. Completion events also published to NATS subject `download.completed`."),
        (name = "search", description = "Search and download. Also available via NATS subject `search_request`."),
        (name = "want", description = "Want requests (search + watch). Also available via NATS subject `want_request`; responses published to `want.response.{source_system}`."),
        (name = "watch", description = "Watch patterns and matches. Also available via NATS subjects `watch_pattern_upsert` and `watch_pattern_delete`. Match events published to `download.watch_matched`."),
        (name = "settings", description = "Indexers, clients, profiles"),
        (name = "system", description = "Health, webhooks, API info"),
    )
)]
struct ApiDoc;

struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Components::default);
        components.add_security_scheme(
            "bearer_auth",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .description(Some("Session token from /api/v1/auth/login"))
                    .build(),
            ),
        );
        components.add_security_scheme(
            "api_key",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::new("Authorization"))),
        );
    }
}

struct DescriptionAddon;

impl Modify for DescriptionAddon {
    fn modify(&self, openapi: &mut openapi::OpenApi) {
        openapi.info.description = Some(API_DESCRIPTION.to_string());
    }
}

/// Build the OpenAPI spec with programmatic path definitions.
fn build_spec() -> openapi::OpenApi {
    let mut doc = ApiDoc::openapi();

    let mut add = |method: HttpMethod, path: &str, tag: &str, summary: &str| {
        let op = OperationBuilder::new()
            .tag(tag)
            .summary(Some(summary))
            .responses(
                ResponsesBuilder::new()
                    .response("200", ResponseBuilder::new().description("Success"))
                    .build(),
            )
            .build();
        let item = PathItemBuilder::new().operation(method, op).build();
        doc.paths.paths.insert(path.to_string(), item);
    };

    // Auth
    add(
        HttpMethod::Post,
        "/api/v1/auth/login",
        "auth",
        "Login with email and password",
    );
    add(
        HttpMethod::Post,
        "/api/v1/auth/logout",
        "auth",
        "Logout current session",
    );
    add(
        HttpMethod::Get,
        "/api/v1/auth/me",
        "auth",
        "Get current user info",
    );

    // Downloads
    add(
        HttpMethod::Get,
        "/api/v1/downloads",
        "downloads",
        "List all downloads",
    );
    add(
        HttpMethod::Post,
        "/api/v1/downloads",
        "downloads",
        "Create a new download",
    );
    add(
        HttpMethod::Get,
        "/api/v1/downloads/{id}",
        "downloads",
        "Get download details",
    );
    add(
        HttpMethod::Delete,
        "/api/v1/downloads/{id}",
        "downloads",
        "Delete a download",
    );
    add(
        HttpMethod::Get,
        "/api/v1/downloads/{id}/file",
        "downloads",
        "Stream the primary downloaded file",
    );
    add(
        HttpMethod::Get,
        "/api/v1/downloads/{id}/files/{path}",
        "downloads",
        "Stream a specific file by path",
    );
    add(
        HttpMethod::Get,
        "/api/v1/downloads/{id}/events",
        "downloads",
        "Get download event history",
    );
    add(
        HttpMethod::Post,
        "/api/v1/downloads/{id}/complete",
        "downloads",
        "Mark download as complete",
    );
    add(
        HttpMethod::Post,
        "/api/v1/downloads/{id}/imported",
        "downloads",
        "Confirm import (triggers cleanup)",
    );
    add(
        HttpMethod::Post,
        "/api/v1/downloads/{id}/reingest",
        "downloads",
        "Reingest download (resend callback)",
    );
    add(
        HttpMethod::Patch,
        "/api/v1/downloads/{id}/retry",
        "downloads",
        "Retry failed download callback",
    );
    add(
        HttpMethod::Get,
        "/api/v1/downloads/stream",
        "downloads",
        "SSE stream of download updates",
    );

    // Search
    add(
        HttpMethod::Post,
        "/api/v1/search",
        "search",
        "Search indexers for releases (NATS: search_request)",
    );
    add(
        HttpMethod::Post,
        "/api/v1/search-download",
        "search",
        "Search and download best match",
    );

    // Want
    add(
        HttpMethod::Post,
        "/api/v1/want",
        "want",
        "Create a want request (NATS: want_request)",
    );
    add(
        HttpMethod::Post,
        "/api/v1/want/bulk",
        "want",
        "Bulk create want requests (max 100)",
    );
    add(
        HttpMethod::Post,
        "/api/v1/want/{source_system}/{source_id}/confirm",
        "want",
        "Confirm want fulfilled (deletes watch pattern)",
    );

    // Watch
    add(
        HttpMethod::Get,
        "/api/v1/admin/watch/patterns",
        "watch",
        "List watch patterns",
    );
    add(
        HttpMethod::Post,
        "/api/v1/admin/watch/patterns",
        "watch",
        "Create watch pattern (NATS: watch_pattern_upsert)",
    );
    add(
        HttpMethod::Delete,
        "/api/v1/admin/watch/patterns/{id}",
        "watch",
        "Delete watch pattern (NATS: watch_pattern_delete)",
    );
    add(
        HttpMethod::Post,
        "/api/v1/admin/watch/{id}/test",
        "watch",
        "Test pattern against RSS cache (dry run)",
    );
    add(
        HttpMethod::Get,
        "/api/v1/admin/watch/matches",
        "watch",
        "List watch match history",
    );

    // Settings - Indexer Sources
    add(
        HttpMethod::Get,
        "/api/v1/admin/indexer-sources",
        "settings",
        "List indexer sources",
    );
    add(
        HttpMethod::Post,
        "/api/v1/admin/indexer-sources",
        "settings",
        "Create indexer source",
    );
    add(
        HttpMethod::Put,
        "/api/v1/admin/indexer-sources/{id}",
        "settings",
        "Update indexer source",
    );
    add(
        HttpMethod::Delete,
        "/api/v1/admin/indexer-sources/{id}",
        "settings",
        "Delete indexer source",
    );
    add(
        HttpMethod::Post,
        "/api/v1/admin/indexer-sources/{id}/check",
        "settings",
        "Trigger manual indexer check",
    );

    // Settings - Download Clients
    add(
        HttpMethod::Get,
        "/api/v1/admin/download-clients",
        "settings",
        "List download clients",
    );
    add(
        HttpMethod::Post,
        "/api/v1/admin/download-clients",
        "settings",
        "Create download client",
    );
    add(
        HttpMethod::Put,
        "/api/v1/admin/download-clients/{id}",
        "settings",
        "Update download client",
    );
    add(
        HttpMethod::Delete,
        "/api/v1/admin/download-clients/{id}",
        "settings",
        "Delete download client",
    );
    add(
        HttpMethod::Post,
        "/api/v1/admin/download-clients/{id}/test",
        "settings",
        "Test download client connection",
    );

    // Settings - Profiles
    add(
        HttpMethod::Get,
        "/api/v1/admin/profiles",
        "settings",
        "List download profiles",
    );
    add(
        HttpMethod::Post,
        "/api/v1/admin/profiles",
        "settings",
        "Create profile",
    );
    add(
        HttpMethod::Get,
        "/api/v1/admin/profile-types",
        "settings",
        "List profile types",
    );
    add(
        HttpMethod::Get,
        "/api/v1/admin/quality-profiles",
        "settings",
        "List quality profiles",
    );
    add(
        HttpMethod::Post,
        "/api/v1/admin/quality-profiles",
        "settings",
        "Create quality profile",
    );
    add(
        HttpMethod::Delete,
        "/api/v1/admin/quality-profiles/{id}",
        "settings",
        "Delete quality profile",
    );

    // Settings - Auth
    add(
        HttpMethod::Get,
        "/api/v1/admin/users",
        "settings",
        "List users",
    );
    add(
        HttpMethod::Post,
        "/api/v1/admin/users",
        "settings",
        "Create user",
    );
    add(
        HttpMethod::Get,
        "/api/v1/admin/api-keys",
        "settings",
        "List API keys",
    );
    add(
        HttpMethod::Post,
        "/api/v1/admin/api-keys",
        "settings",
        "Create API key",
    );
    add(
        HttpMethod::Delete,
        "/api/v1/admin/api-keys/{id}",
        "settings",
        "Delete API key",
    );

    // System
    add(
        HttpMethod::Get,
        "/api/v1/nats/info",
        "system",
        "NATS job/event discovery (subjects, concurrency, schedules)",
    );
    add(HttpMethod::Get, "/health", "system", "Health check");
    add(
        HttpMethod::Get,
        "/api/v1/info",
        "system",
        "API endpoint listing",
    );
    add(
        HttpMethod::Get,
        "/api/v1/admin/system/health",
        "system",
        "System health dashboard",
    );
    add(
        HttpMethod::Get,
        "/api/v1/admin/webhooks",
        "system",
        "List status webhooks",
    );
    add(
        HttpMethod::Post,
        "/api/v1/admin/webhooks",
        "system",
        "Create status webhook",
    );
    add(
        HttpMethod::Put,
        "/api/v1/admin/webhooks/{id}",
        "system",
        "Update webhook",
    );
    add(
        HttpMethod::Delete,
        "/api/v1/admin/webhooks/{id}",
        "system",
        "Delete webhook",
    );

    doc
}

/// Create routes for serving OpenAPI spec and Swagger UI.
pub fn routes() -> Router<AppState> {
    utoipa_swagger_ui::SwaggerUi::new("/api/v1/docs")
        .url("/api/v1/openapi.json", build_spec())
        .into()
}
