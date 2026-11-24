//! What counts as SAB still holding its connections, and what the engine does when SAB
//! leaves it no room under the account cap.

use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::common::usenet::UsenetRig;
use crate::usenet_sab_share_test::{
    CLIENTS, add_sab, add_server, engine_server, id_of, listed_effective, mock_sab, patch_sab,
    pulled, sab_server,
};
use kobload_api::repositories::app_settings;

fn slot(status: &str, priority: &str) -> Value {
    json!({ "nzo_id": "SABnzbd_nzo_1", "status": status, "priority": priority })
}

async fn delete_sab(rig: &UsenetRig, id: &str) -> (StatusCode, String) {
    let resp = rig
        .tv
        .server
        .admin_client()
        .delete(&format!("{CLIENTS}/{id}"))
        .await;
    (resp.status, resp.text())
}

#[tokio::test]
async fn test_a_disabled_sab_drains_only_while_it_can_still_be_working() {
    let rig = UsenetRig::start().await;
    add_server(&rig, &engine_server("news.a.test", None, 49, 50)).await;
    let (url, sab) = mock_sab(json!([sab_server("news.a.test", "alice", 40)])).await;
    let (_, body) = add_sab(&rig, &url, false).await;
    id_of(&body);
    let (held, free) = (10, 49);
    for (paused, slots, expected) in [
        (false, vec![slot("Downloading", "Normal")], held),
        (false, vec![slot("Queued", "Normal")], held),
        (false, vec![slot("Fetching", "Normal")], held),
        (false, vec![slot("Checking", "Normal")], held),
        (false, vec![slot("Propagating", "Normal")], held),
        (false, vec![slot("Paused", "Normal")], free),
        (false, vec![slot("Grabbing", "Normal")], free),
        (
            false,
            vec![slot("Paused", "Normal"), slot("Downloading", "Normal")],
            held,
        ),
        (true, vec![slot("Downloading", "Normal")], free),
        (true, vec![slot("Queued", "High")], free),
        (true, vec![slot("Downloading", "Force")], held),
        (true, vec![slot("Paused", "Force")], free),
        (true, vec![slot("Grabbing", "Force")], free),
    ] {
        *sab.paused.lock().unwrap() = paused;
        *sab.queue.lock().unwrap() = Value::Array(slots.clone());
        assert_eq!(
            pulled(&rig).await,
            [("news.a.test".to_string(), expected)],
            "paused={paused} {slots:?}"
        );
    }
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_deleting_a_sab_mid_queue_is_refused_until_it_drains() {
    let rig = UsenetRig::start().await;
    add_server(&rig, &engine_server("news.a.test", None, 49, 50)).await;
    let (url, sab) = mock_sab(json!([sab_server("news.a.test", "alice", 40)])).await;
    let (_, body) = add_sab(&rig, &url, true).await;
    let id = id_of(&body);
    sab.busy(true);
    let before = rig.engine.config_refreshes();

    let (status, body) = delete_sab(&rig, &id).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("queue"), "{body}");
    assert!(!body.contains("alice"), "{body}");
    assert_eq!(rig.engine.config_refreshes(), before);
    assert_eq!(pulled(&rig).await, [("news.a.test".to_string(), 10)]);

    sab.busy(false);
    let (status, body) = delete_sab(&rig, &id).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(pulled(&rig).await, [("news.a.test".to_string(), 49)]);
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_a_sab_that_holds_nothing_or_is_dead_can_be_deleted() {
    let rig = UsenetRig::start().await;
    add_server(&rig, &engine_server("news.a.test", None, 49, 50)).await;
    for dead in [false, true] {
        let (url, sab) = mock_sab(json!([sab_server("news.a.test", "alice", 40)])).await;
        let (_, body) = add_sab(&rig, &url, true).await;
        let id = id_of(&body);
        if dead {
            sab.go_down();
        } else {
            *sab.queue.lock().unwrap() = json!([slot("Paused", "Normal")]);
        }
        let (status, body) = delete_sab(&rig, &id).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "dead={dead}: {body}");
    }
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_a_server_with_no_room_gets_nothing_and_leaves_the_engine_config() {
    let rig = UsenetRig::start().await;
    add_server(&rig, &engine_server("news.a.test", None, 10, 50)).await;
    add_server(&rig, &engine_server("news.other.test", None, 20, 30)).await;
    let (url, sab) = mock_sab(json!([sab_server("news.a.test", "alice", 40)])).await;
    let (status, body) = add_sab(&rig, &url, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let other = ("news.other.test".to_string(), 20);
    assert_eq!(
        pulled(&rig).await,
        [("news.a.test".to_string(), 10), other.clone()]
    );

    for filled in [50, 60] {
        *sab.servers.lock().unwrap() = json!([sab_server("news.a.test", "alice", filled)]);
        assert_eq!(
            pulled(&rig).await,
            std::slice::from_ref(&other),
            "SAB holds {filled}"
        );
        assert_eq!(listed_effective(&rig, "news.a.test").await, 0);
    }

    *sab.servers.lock().unwrap() = json!([sab_server("news.a.test", "alice", 40)]);
    assert_eq!(pulled(&rig).await, [("news.a.test".to_string(), 10), other]);
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_the_last_connection_goes_to_the_higher_priority_server() {
    let rig = UsenetRig::start().await;
    let mut second = engine_server("eu.news.a.test", Some("omicron"), 10, 30);
    second["priority"] = json!(1);
    add_server(&rig, &engine_server("news.a.test", Some("omicron"), 10, 30)).await;
    add_server(&rig, &second).await;
    let (url, sab) = mock_sab(json!([sab_server("news.a.test", "alice", 20)])).await;
    add_sab(&rig, &url, true).await;

    *sab.servers.lock().unwrap() = json!([sab_server("news.a.test", "alice", 29)]);
    assert_eq!(pulled(&rig).await, [("news.a.test".to_string(), 1)]);
    assert_eq!(listed_effective(&rig, "eu.news.a.test").await, 0);
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_the_refusal_no_longer_promises_the_engine_a_connection() {
    let rig = UsenetRig::start().await;
    add_server(&rig, &engine_server("news.a.test", None, 10, 50)).await;
    let (url, _) = mock_sab(json!([sab_server("news.a.test", "alice", 50)])).await;
    let (status, body) = add_sab(&rig, &url, true).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(!body.contains("at least"), "{body}");
    assert!(body.contains("no connection"), "{body}");
    assert!(!body.contains("alice"), "{body}");
    rig.tv.server.cleanup().await;
}

async fn expire_trust(rig: &UsenetRig, id: &str) {
    let key = format!("usenet_sab_share:{id}");
    let pool = &rig.tv.server.pool;
    let mut share = app_settings::get(pool, &key).await.unwrap().unwrap();
    share["seen_at"] = json!(time::OffsetDateTime::now_utc().unix_timestamp() - 3601);
    app_settings::set(pool, &key, &share).await.unwrap();
}

#[tokio::test]
async fn test_a_sab_lost_while_unreachable_holds_its_share_for_the_trust_period() {
    #[derive(Clone, Copy, Debug)]
    enum Lost {
        Deleted,
        Disabled,
        DisabledAfterQueueFailed,
    }
    for lost in [
        Lost::Deleted,
        Lost::Disabled,
        Lost::DisabledAfterQueueFailed,
    ] {
        let rig = UsenetRig::start().await;
        add_server(&rig, &engine_server("news.a.test", None, 49, 50)).await;
        let (url, sab) = mock_sab(json!([sab_server("news.a.test", "alice", 40)])).await;
        let (_, body) = add_sab(&rig, &url, true).await;
        let id = id_of(&body);
        assert_eq!(pulled(&rig).await, [("news.a.test".to_string(), 10)]);

        if matches!(lost, Lost::DisabledAfterQueueFailed) {
            *sab.queue.lock().unwrap() = Value::Null;
            assert_eq!(pulled(&rig).await, [("news.a.test".to_string(), 10)]);
        }
        sab.go_down();
        match lost {
            Lost::Deleted => {
                let (status, body) = delete_sab(&rig, &id).await;
                assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
            }
            Lost::Disabled | Lost::DisabledAfterQueueFailed => {
                let (status, body) = patch_sab(&rig, &id, &json!({ "enabled": false })).await;
                assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
            }
        }
        assert_eq!(
            pulled(&rig).await,
            [("news.a.test".to_string(), 10)],
            "{lost:?}: inside the trust period"
        );

        expire_trust(&rig, &id).await;
        assert_eq!(
            pulled(&rig).await,
            [("news.a.test".to_string(), 49)],
            "{lost:?}: past the trust period"
        );
        rig.tv.server.cleanup().await;
    }
}

#[tokio::test]
async fn test_deleting_a_dead_sab_that_held_nothing_frees_the_engine_at_once() {
    let rig = UsenetRig::start().await;
    add_server(&rig, &engine_server("news.a.test", None, 49, 50)).await;
    let (url, sab) = mock_sab(json!([sab_server("news.a.test", "alice", 40)])).await;
    let (_, body) = add_sab(&rig, &url, false).await;
    let id = id_of(&body);
    assert_eq!(pulled(&rig).await, [("news.a.test".to_string(), 49)]);

    sab.go_down();
    let (status, body) = delete_sab(&rig, &id).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(pulled(&rig).await, [("news.a.test".to_string(), 49)]);
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_re_adding_a_deleted_dead_sab_does_not_count_it_twice() {
    let rig = UsenetRig::start().await;
    add_server(&rig, &engine_server("news.a.test", None, 49, 90)).await;
    let (url, sab) = mock_sab(json!([sab_server("news.a.test", "alice", 40)])).await;
    let (_, body) = add_sab(&rig, &url, true).await;
    let id = id_of(&body);
    let servers = sab.servers.lock().unwrap().clone();
    sab.go_down();
    delete_sab(&rig, &id).await;

    *sab.servers.lock().unwrap() = servers;
    sab.busy(false);
    let (status, body) = add_sab(&rig, &url, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(pulled(&rig).await, [("news.a.test".to_string(), 49)]);
    rig.tv.server.cleanup().await;
}
