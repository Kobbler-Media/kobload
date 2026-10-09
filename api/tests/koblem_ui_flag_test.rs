//! Koblem's nav entry is hidden unless explicitly opted into via KOBLEM_UI_ENABLED,
//! since it's wired for one specific hardware setup with no non-DB way to edit its
//! GPU gating. This guards the default-off behaviour the frontend relies on.

mod common;

use common::TestServer;
use serde_json::Value;

#[tokio::test]
async fn koblem_ui_is_disabled_by_default() {
    assert!(
        std::env::var("KOBLEM_UI_ENABLED").is_err(),
        "test process must not inherit KOBLEM_UI_ENABLED for this assertion to be meaningful"
    );

    let server = TestServer::start().await;

    let resp = server
        .admin_client()
        .get("/api/v1/admin/dev/snapshot")
        .await;

    resp.assert_ok();
    let json: Value = resp.json();
    assert_eq!(json["koblem_enabled"], Value::Bool(false));

    server.cleanup().await;
}
