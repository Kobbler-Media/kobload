//! Quality profile API tests.

mod common;

use reqwest::StatusCode;
use serde_json::{Value, json};

use common::TestServer;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a quality profile via the API and return the created profile's id.
async fn create_profile(
    server: &TestServer,
    name: &str,
    media_kind: &str,
    is_default: bool,
) -> String {
    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/admin/quality-profiles",
            &json!({
                "name": name,
                "media_kind": media_kind,
                "is_default": is_default,
            }),
        )
        .await;
    resp.assert_ok();
    let body: Value = resp.json();
    body["id"].as_str().expect("profile id").to_string()
}

/// GET /api/v1/admin/quality-profiles and return the parsed list.
async fn list_profiles(server: &TestServer) -> Vec<Value> {
    let resp = server
        .admin_client()
        .get("/api/v1/admin/quality-profiles")
        .await;
    resp.assert_ok();
    resp.json()
}

/// Find a profile by id in a list.
fn find_profile<'a>(profiles: &'a [Value], id: &str) -> &'a Value {
    profiles
        .iter()
        .find(|p| p["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("profile {} not found in list", id))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_quality_profile_crud() {
    let server = TestServer::start().await;

    let create = server
        .admin_client()
        .post_json(
            "/api/v1/admin/quality-profiles",
            &json!({
                "name": "Movies - HD",
                "media_kind": "movie",
                "min_resolution": "720p",
                "preferred_sources": ["web-dl"],
                "preferred_codecs": ["x265"],
                "preferred_audio": ["dts"]
            }),
        )
        .await;
    create.assert_ok();
    let created: Value = create.json();
    let id = created["id"].as_str().expect("id");

    let profiles = list_profiles(&server).await;
    assert!(profiles.iter().any(|p| p["id"] == id));

    let update = server
        .admin_client()
        .patch_json(
            &format!("/api/v1/admin/quality-profiles/{}", id),
            &json!({
                "max_resolution": "1080p"
            }),
        )
        .await;
    update.assert_status(StatusCode::NO_CONTENT);

    let delete = server
        .admin_client()
        .delete(&format!("/api/v1/admin/quality-profiles/{}", id))
        .await;
    delete.assert_status(StatusCode::NO_CONTENT);

    server.cleanup().await;
}

/// Creating a profile with `is_default: true` should persist the flag.
#[tokio::test]
async fn test_create_profile_with_is_default() {
    let server = TestServer::start().await;

    let id = create_profile(&server, "Movies - Default", "movie", true).await;

    let profiles = list_profiles(&server).await;
    let profile = find_profile(&profiles, &id);
    assert_eq!(
        profile["is_default"].as_bool(),
        Some(true),
        "newly created profile should have is_default = true"
    );

    server.cleanup().await;
}

/// When two profiles of the same media_kind are created with `is_default: true`,
/// only the most recently created one should remain the default.
/// A default for a different media_kind should be unaffected.
#[tokio::test]
async fn test_only_one_default_per_media_kind() {
    let server = TestServer::start().await;

    // Create a "tv" default to prove cross-media-kind isolation.
    let tv_id = create_profile(&server, "TV - Default", "tv", true).await;

    // Create two "movie" defaults in sequence.
    let movie_a = create_profile(&server, "Movie A", "movie", true).await;
    let movie_b = create_profile(&server, "Movie B", "movie", true).await;

    let profiles = list_profiles(&server).await;

    // Movie A should have been dethroned.
    assert_eq!(
        find_profile(&profiles, &movie_a)["is_default"].as_bool(),
        Some(false),
        "movie A should no longer be default after movie B was created as default"
    );
    // Movie B is the new default.
    assert_eq!(
        find_profile(&profiles, &movie_b)["is_default"].as_bool(),
        Some(true),
        "movie B should be the default"
    );
    // TV default should be untouched.
    assert_eq!(
        find_profile(&profiles, &tv_id)["is_default"].as_bool(),
        Some(true),
        "tv default should be unaffected by movie default changes"
    );

    server.cleanup().await;
}

/// PATCHing a profile to `is_default: true` should clear the previous default
/// for the same media_kind.
#[tokio::test]
async fn test_update_profile_to_default_clears_previous() {
    let server = TestServer::start().await;

    let first = create_profile(&server, "TV First", "tv", true).await;
    let second = create_profile(&server, "TV Second", "tv", false).await;

    // Sanity: first is default, second is not.
    let profiles = list_profiles(&server).await;
    assert_eq!(
        find_profile(&profiles, &first)["is_default"].as_bool(),
        Some(true)
    );
    assert_eq!(
        find_profile(&profiles, &second)["is_default"].as_bool(),
        Some(false)
    );

    // PATCH second to become the default.
    let patch = server
        .admin_client()
        .patch_json(
            &format!("/api/v1/admin/quality-profiles/{}", second),
            &json!({ "is_default": true }),
        )
        .await;
    patch.assert_status(StatusCode::NO_CONTENT);

    let profiles = list_profiles(&server).await;
    assert_eq!(
        find_profile(&profiles, &first)["is_default"].as_bool(),
        Some(false),
        "first profile should no longer be default after second was patched"
    );
    assert_eq!(
        find_profile(&profiles, &second)["is_default"].as_bool(),
        Some(true),
        "second profile should now be default"
    );

    server.cleanup().await;
}

/// After seeding (via the reset endpoint), exactly one TV and one Movie profile
/// should be marked as default, and they should be the 1080p presets.
#[tokio::test]
async fn test_seeded_defaults_are_1080p() {
    let server = TestServer::start().await;

    // Use the reset endpoint which deletes all profiles and re-seeds presets.
    let reset = server
        .admin_client()
        .post("/api/v1/admin/quality-profiles/reset")
        .await;
    reset.assert_ok();

    let profiles = list_profiles(&server).await;

    // TV defaults
    let tv_defaults: Vec<&Value> = profiles
        .iter()
        .filter(|p| {
            p["media_kind"].as_str() == Some("tv") && p["is_default"].as_bool() == Some(true)
        })
        .collect();
    assert_eq!(
        tv_defaults.len(),
        1,
        "expected exactly one default TV profile, got {}",
        tv_defaults.len()
    );
    assert_eq!(
        tv_defaults[0]["name"].as_str(),
        Some("TV - 1080p"),
        "default TV profile should be 'TV - 1080p'"
    );

    // Movie defaults
    let movie_defaults: Vec<&Value> = profiles
        .iter()
        .filter(|p| {
            p["media_kind"].as_str() == Some("movie") && p["is_default"].as_bool() == Some(true)
        })
        .collect();
    assert_eq!(
        movie_defaults.len(),
        1,
        "expected exactly one default movie profile, got {}",
        movie_defaults.len()
    );
    assert_eq!(
        movie_defaults[0]["name"].as_str(),
        Some("Movies - 1080p"),
        "default movie profile should be 'Movies - 1080p'"
    );

    server.cleanup().await;
}
