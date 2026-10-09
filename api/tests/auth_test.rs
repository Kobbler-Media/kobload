//! Authentication API tests.

mod common;

use reqwest::StatusCode;
use serde_json::{Value, json};

use common::{TestServer, create_api_key, create_user};

#[tokio::test]
async fn test_login_success() {
    let server = TestServer::start().await;

    // Use the pre-created admin user
    let resp = server
        .anon_client()
        .post_json(
            "/api/v1/admin/login",
            &json!({
                "email": "admin@test.local",
                "password": "testadmin123"
            }),
        )
        .await;

    resp.assert_ok();
    let json: Value = resp.json();
    assert!(json["token"].is_string());

    server.cleanup().await;
}

#[tokio::test]
async fn test_login_invalid_password() {
    let server = TestServer::start().await;

    let resp = server
        .anon_client()
        .post_json(
            "/api/v1/admin/login",
            &json!({
                "email": "admin@test.local",
                "password": "wrongpassword"
            }),
        )
        .await;

    resp.assert_status(StatusCode::UNAUTHORIZED);

    server.cleanup().await;
}

#[tokio::test]
async fn test_login_nonexistent_user() {
    let server = TestServer::start().await;

    let resp = server
        .anon_client()
        .post_json(
            "/api/v1/admin/login",
            &json!({
                "email": "nonexistent@test.local",
                "password": "password123"
            }),
        )
        .await;

    resp.assert_status(StatusCode::UNAUTHORIZED);

    server.cleanup().await;
}

#[tokio::test]
async fn test_create_user() {
    let server = TestServer::start().await;

    let user_id = create_user(&server, "newuser@test.local", "password123").await;
    assert!(!user_id.is_nil());

    // Verify the user can login
    let resp = server
        .anon_client()
        .post_json(
            "/api/v1/admin/login",
            &json!({
                "email": "newuser@test.local",
                "password": "password123"
            }),
        )
        .await;

    resp.assert_ok();

    server.cleanup().await;
}

#[tokio::test]
async fn test_create_user_requires_auth() {
    let server = TestServer::start().await;

    let resp = server
        .anon_client()
        .post_json(
            "/api/v1/admin/users",
            &json!({
                "email": "newuser@test.local",
                "password": "password123"
            }),
        )
        .await;

    resp.assert_status(StatusCode::UNAUTHORIZED);

    server.cleanup().await;
}

#[tokio::test]
async fn test_create_duplicate_user_fails() {
    let server = TestServer::start().await;

    // Create first user
    create_user(&server, "duplicate@test.local", "password123").await;

    // Try to create duplicate
    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/admin/users",
            &json!({
                "email": "duplicate@test.local",
                "password": "password123"
            }),
        )
        .await;

    resp.assert_status(StatusCode::BAD_REQUEST);

    server.cleanup().await;
}

#[tokio::test]
async fn test_list_users() {
    let server = TestServer::start().await;

    // Create some users
    create_user(&server, "user1@test.local", "password123").await;
    create_user(&server, "user2@test.local", "password123").await;

    let resp = server.admin_client().get("/api/v1/admin/users").await;
    resp.assert_ok();

    let users: Vec<Value> = resp.json();
    // Should have admin + 2 created users
    assert!(users.len() >= 3);

    server.cleanup().await;
}

#[tokio::test]
async fn test_list_users_requires_auth() {
    let server = TestServer::start().await;

    let resp = server.anon_client().get("/api/v1/admin/users").await;
    resp.assert_status(StatusCode::UNAUTHORIZED);

    server.cleanup().await;
}

#[tokio::test]
async fn test_update_user_email() {
    let server = TestServer::start().await;

    let user_id = create_user(&server, "original@test.local", "password123").await;

    let resp = server
        .admin_client()
        .patch_json(
            &format!("/api/v1/admin/users/{}", user_id),
            &json!({
                "email": "updated@test.local"
            }),
        )
        .await;

    resp.assert_ok();
    let json: Value = resp.json();
    assert_eq!(json["email"], "updated@test.local");

    // Verify can login with new email
    let resp = server
        .anon_client()
        .post_json(
            "/api/v1/admin/login",
            &json!({
                "email": "updated@test.local",
                "password": "password123"
            }),
        )
        .await;
    resp.assert_ok();

    server.cleanup().await;
}

#[tokio::test]
async fn test_update_user_password() {
    let server = TestServer::start().await;

    let user_id = create_user(&server, "pwchange@test.local", "oldpassword").await;

    let resp = server
        .admin_client()
        .patch_json(
            &format!("/api/v1/admin/users/{}", user_id),
            &json!({
                "password": "newpassword"
            }),
        )
        .await;

    resp.assert_ok();

    // Old password should fail
    let resp = server
        .anon_client()
        .post_json(
            "/api/v1/admin/login",
            &json!({
                "email": "pwchange@test.local",
                "password": "oldpassword"
            }),
        )
        .await;
    resp.assert_status(StatusCode::UNAUTHORIZED);

    // New password should work
    let resp = server
        .anon_client()
        .post_json(
            "/api/v1/admin/login",
            &json!({
                "email": "pwchange@test.local",
                "password": "newpassword"
            }),
        )
        .await;
    resp.assert_ok();

    server.cleanup().await;
}

#[tokio::test]
async fn test_api_key_crud() {
    let server = TestServer::start().await;

    // Create API key
    let (key_id, key) = create_api_key(&server, "Test Key").await;
    assert!(!key_id.is_nil());
    assert!(!key.is_empty());

    // List API keys
    let resp = server.admin_client().get("/api/v1/admin/api-keys").await;
    resp.assert_ok();
    let keys: Vec<Value> = resp.json();
    assert!(keys.iter().any(|k| k["name"] == "Test Key"));

    // Use the API key to authenticate
    let resp = server
        .anon_client()
        .with_auth(&key)
        .get("/api/v1/admin/users")
        .await;
    resp.assert_ok();

    // Revoke the key
    let resp = server
        .admin_client()
        .delete(&format!("/api/v1/admin/api-keys/{}", key_id))
        .await;
    resp.assert_status(StatusCode::NO_CONTENT);

    // Key should no longer work
    let resp = server
        .anon_client()
        .with_auth(&key)
        .get("/api/v1/admin/users")
        .await;
    resp.assert_status(StatusCode::UNAUTHORIZED);

    server.cleanup().await;
}

#[tokio::test]
async fn test_api_key_requires_name() {
    let server = TestServer::start().await;

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/admin/api-keys",
            &json!({
                "name": ""
            }),
        )
        .await;

    resp.assert_status(StatusCode::BAD_REQUEST);

    server.cleanup().await;
}
