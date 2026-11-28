//! The debrid log groups stream attempts under the search that listed the release, with the
//! search's ranked options and why each ranked where it did.

use std::time::Duration;

use serde_json::{Value, json};

use crate::common::debrid_mock::MockDebrid;
use crate::common::stream_fixtures::hash;
use crate::common::tv_watch::{TvWatch, episode_want, torrent_release};
use kobload_api::jobs::handlers::CacheCleanupHandler;
use kobload_api::jobs::worker::JobHandler;
use kobload_api::repositories::app_settings;
use kobload_api::repositories::stream_searches::{self, SearchOption};
use tokio_util::sync::CancellationToken;

struct Rig {
    tv: TvWatch,
    provider: uuid::Uuid,
}

impl Rig {
    async fn start() -> Self {
        let mock = MockDebrid::new(Duration::from_millis(1));
        let tv = TvWatch::start_with_debrid(mock.resolver()).await;
        app_settings::set(
            &tv.server.pool,
            app_settings::keys::DEBRID_LOGGING_ENABLED,
            &json!(true),
        )
        .await
        .unwrap();
        let provider = tv.debrid_provider().await;
        tv.indexer.set_search_results(vec![
            torrent_release("t-1", "Group.Show.S01E05.1080p.WEB-DL.x264-AAA", 'a'),
            torrent_release("t-2", "Group.Show.S01E05.720p.WEB-DL.x264-BBB", 'b'),
        ]);
        Self { tv, provider }
    }

    async fn search(&self) {
        self.tv
            .stream_options(&episode_want("Group Show", "1", "5", None))
            .await;
    }

    async fn groups(&self) -> Vec<Value> {
        let resp = self
            .tv
            .server
            .admin_client()
            .get("/api/v1/stream-searches")
            .await;
        resp.assert_ok();
        resp.json()
    }

    /// Warming runs after the search returns.
    async fn settled_groups(&self, attempts: usize) -> Vec<Value> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let groups = self.groups().await;
            let settled = groups.first().is_some_and(|g| {
                g["attempts"].as_array().is_some_and(|a| {
                    a.len() == attempts && a.iter().all(|a| a["status"] != "resolving")
                })
            });
            if settled {
                return groups;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "attempts never settled: {groups:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn resolve(&self, release_key: &str) {
        self.tv
            .server
            .admin_client()
            .post_json(
                "/api/v1/stream/resolve",
                &json!({"debrid_provider_id": self.provider, "release_key": release_key}),
            )
            .await
            .assert_ok();
    }
}

#[tokio::test]
async fn test_a_search_lists_its_ranked_options_and_the_attempts_on_them() {
    let rig = Rig::start().await;
    rig.search().await;
    let groups = rig.settled_groups(2).await;
    assert_eq!(groups.len(), 1, "{groups:?}");
    let search = &groups[0]["search"];
    assert_eq!(search["title"], "Group Show S01E05");
    let options = search["options"].as_array().unwrap();
    let keys: Vec<&str> = options
        .iter()
        .map(|o| o["release_key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, [hash('a'), hash('b')]);
    for (i, option) in options.iter().enumerate() {
        assert_eq!(option["rank"], i + 1);
        assert!(
            option["why"].as_array().is_some_and(|w| !w.is_empty()),
            "{option}"
        );
    }
    for attempt in groups[0]["attempts"].as_array().unwrap() {
        assert_eq!(attempt["origin"], "warm");
        assert_eq!(attempt["search_id"], search["id"]);
    }

    rig.resolve(&hash('a')).await;
    let groups = rig.settled_groups(3).await;
    assert_eq!(
        groups.len(),
        1,
        "a play joins the search that listed it: {groups:?}"
    );
    let play = groups[0]["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["origin"] == "play")
        .expect("play attempt");
    assert_eq!(play["release_key"], hash('a'));
    rig.tv.server.cleanup().await;
}

// One play asks more than once (the page's own search, the server's prewarm, the library page
// the title turns into); each used to be another card and another round of warming.
#[tokio::test]
async fn test_a_repeat_of_a_recent_search_lists_under_it_and_warms_nothing() {
    let rig = Rig::start().await;
    rig.search().await;
    let first = rig.settled_groups(2).await;
    rig.search().await;
    rig.search().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let groups = rig.settled_groups(2).await;
    assert_eq!(groups.len(), 1, "{groups:?}");
    assert_eq!(groups[0]["search"]["id"], first[0]["search"]["id"]);
    rig.tv.server.cleanup().await;
}

// Warming verifying a copy can move it up the ranking, so a repeat may list the same copies
// in another order.
#[tokio::test]
async fn test_a_repeat_listing_the_same_copies_reordered_is_still_a_repeat() {
    let rig = Rig::start().await;
    let pool = &rig.tv.server.pool;
    let option = |key: char| SearchOption {
        rank: 1,
        release_key: Some(hash(key)),
        file_id: None,
        release_title: format!("Release {key}"),
        size: 1,
        indexer: "mock".into(),
        source_type: "torrent".into(),
        group: None,
        availability: None,
        total_score: 1.0,
        why: vec![],
    };
    let first = uuid::Uuid::new_v4();
    stream_searches::insert(pool, first, "Movie (2020)", &[option('a'), option('b')])
        .await
        .unwrap();
    let repeat = stream_searches::refresh(pool, "Movie (2020)", &[option('b'), option('a')], 120)
        .await
        .unwrap();
    assert_eq!(repeat.map(|s| s.id), Some(first));
    let other = stream_searches::refresh(pool, "Movie (2020)", &[option('a'), option('c')], 120)
        .await
        .unwrap();
    assert!(other.is_none(), "a different listing is a new search");
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_a_play_no_search_listed_stands_alone() {
    let rig = Rig::start().await;
    rig.resolve(&hash('z')).await;
    let groups = rig.groups().await;
    assert_eq!(groups.len(), 1, "{groups:?}");
    assert!(groups[0]["search"].is_null());
    assert_eq!(groups[0]["attempts"][0]["release_key"], hash('z'));
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_clearing_and_cleanup_remove_searches_with_their_attempts() {
    let rig = Rig::start().await;
    rig.search().await;
    rig.settled_groups(2).await;
    let pool = &rig.tv.server.pool;
    sqlx::query("UPDATE stream_searches SET created_at = NOW() - INTERVAL '25 hours'")
        .execute(pool)
        .await
        .unwrap();
    CacheCleanupHandler::new(pool.clone())
        .execute(uuid::Uuid::new_v4(), json!({}), CancellationToken::new())
        .await
        .unwrap();
    let searches: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM stream_searches")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(searches, 0);

    rig.search().await;
    // Both releases were warmed by the first search, so this one probes nothing.
    rig.settled_groups(0).await;
    rig.tv
        .server
        .admin_client()
        .delete("/api/v1/stream-attempts")
        .await
        .assert_ok();
    assert!(rig.groups().await.is_empty());
    rig.tv.server.cleanup().await;
}
