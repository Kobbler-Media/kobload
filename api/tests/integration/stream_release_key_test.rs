//! Streams are keyed by `release_key`: a torrent's info hash, or `nzb-<sha256>` once an NZB
//! is imported. Usenet options travel as an opaque `nzb_ref` that only kobload dereferences,
//! so an indexer's API key never leaves it.

use std::time::Duration;

use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::common::debrid_mock::MockDebrid;
use crate::common::stream_fixtures::hash;
use crate::common::tv_watch::{TvWatch, episode_want, torrent_release, usenet_release};
use kobload_api::repositories::usenet_releases;
use kobload_api::watch::Release;
use nats_jobs::NzbRef;
use nats_jobs::types::{StreamOption, StreamPlayback, StreamSourceType};

const SECRET: &str = "s3cr3t-indexer-key";

struct Rig {
    tv: TvWatch,
    mock: std::sync::Arc<MockDebrid>,
    provider: Uuid,
    show: &'static str,
}

impl Rig {
    async fn start(show: &'static str, releases: Vec<Release>) -> Self {
        let mock = MockDebrid::new(Duration::from_millis(1));
        let tv = TvWatch::start_with_debrid(mock.resolver()).await;
        let provider = crate::common::stream_fixtures::create_provider(&tv.server.pool, "torbox")
            .await
            .id;
        tv.indexer.set_search_results(releases);
        Self {
            tv,
            mock,
            provider,
            show,
        }
    }

    fn nzb(&self, guid: &str, title: &str, indexer: Option<Uuid>) -> Release {
        let mut release = usenet_release(guid, title);
        release.download_url = Some(nzb_url(guid));
        release.indexer_source_id = Some(indexer.unwrap_or(self.tv.source.id));
        release
    }

    async fn options(&self) -> Vec<StreamOption> {
        self.tv
            .stream_options(&episode_want(self.show, "1", "5", None))
            .await
    }

    async fn post(&self, path: &str, body: Value) -> (StatusCode, String) {
        let resp = self.tv.server.admin_client().post_json(path, &body).await;
        (resp.status, resp.text())
    }

    async fn report(&self, release_key: &str, ok: bool) {
        let (status, body) = self
            .post(
                "/api/v1/stream/outcome",
                json!({ "debrid_provider_id": self.provider, "release_key": release_key, "ok": ok,
                        "reason": "decoder_error" }),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    }
}

fn nzb_url(guid: &str) -> String {
    format!("https://indexer.invalid/api?t=get&id={guid}&apikey={SECRET}")
}

fn usenet(options: &[StreamOption]) -> Vec<&StreamOption> {
    options
        .iter()
        .filter(|o| o.source_type == StreamSourceType::Usenet)
        .collect()
}

#[tokio::test]
async fn test_stream_search_lists_usenet_by_reference_and_never_leaks_the_indexer_url() {
    let rig = Rig::start("Ref Leak Show", Vec::new()).await;
    rig.tv.indexer.set_search_results(vec![
        rig.nzb("g-1", "Ref.Leak.Show.S01E05.1080p.WEB-DL.x264-NZB", None),
        torrent_release("t-1", "Ref.Leak.Show.S01E05.720p.WEB-DL.x264-TOR", 'a'),
    ]);
    let options = rig.options().await;

    let nzb = usenet(&options);
    assert_eq!(nzb.len(), 1, "{options:?}");
    assert_eq!(
        nzb[0].nzb_ref,
        Some(NzbRef {
            indexer_id: rig.tv.source.id.to_string(),
            guid: "g-1".to_string(),
        })
    );
    assert_eq!(nzb[0].release_key, None);
    let torrent = options
        .iter()
        .find(|o| o.source_type == StreamSourceType::Torrent)
        .expect("torrent option");
    assert_eq!(torrent.release_key.as_deref(), Some(hash('a').as_str()));

    let stored = usenet_releases::get(&rig.tv.server.pool, nzb[0].nzb_ref.as_ref().unwrap())
        .await
        .unwrap()
        .expect("listed usenet release is remembered");
    assert_eq!(stored.download_url, nzb_url("g-1"));

    let (status, body) = rig
        .post(
            "/api/v1/stream/search",
            json!({ "media_want": episode_want(rig.show, "1", "5", None), "media_type": "tv",
                    "include_uncached": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for leak in [
        SECRET,
        "indexer.invalid",
        "nzb_url",
        "download_url",
        "info_hash",
    ] {
        assert!(
            !body.contains(leak),
            "search response carries {leak}: {body}"
        );
    }
    assert!(body.contains("\"nzb_ref\""), "{body}");
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_resolving_an_nzb_ref_hands_the_indexer_url_to_the_provider() {
    let rig = Rig::start("Ref Resolve Show", Vec::new()).await;
    rig.tv.indexer.set_search_results(vec![rig.nzb(
        "g-2",
        "Ref.Resolve.Show.S01E05.1080p.WEB-DL.x264-NZB",
        None,
    )]);
    let options = rig.options().await;
    let nzb_ref = usenet(&options)[0].nzb_ref.clone().expect("nzb_ref");

    let (status, body) = rig
        .post(
            "/api/v1/stream/resolve",
            json!({ "debrid_provider_id": rig.provider, "nzb_ref": nzb_ref }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let resolved: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(resolved["filename"], "usenet.mkv");
    assert!(!body.contains(SECRET), "{body}");
    let adds: Vec<String> = rig
        .mock
        .calls()
        .into_iter()
        .filter(|c| c.op == "add_nzb")
        .map(|c| c.hash)
        .collect();
    assert_eq!(adds, vec![nzb_url("g-2")]);

    let (status, body) = rig
        .post(
            "/api/v1/stream/resolve",
            json!({ "debrid_provider_id": rig.provider,
                    "nzb_ref": { "indexer_id": "nobody", "guid": "unlisted" } }),
        )
        .await;
    assert!(!status.is_success(), "an unlisted ref resolved: {body}");
    assert_eq!(rig.mock.count("add_nzb", None), 1);
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_a_torrent_is_keyed_by_its_info_hash_through_resolve_attempts_and_outcomes() {
    let rig = Rig::start("Key Torrent Show", Vec::new()).await;
    let h = hash('k');
    rig.tv.indexer.set_search_results(vec![torrent_release(
        "t-k",
        "Key.Torrent.Show.S01E05.1080p.WEB-DL.x264-TOR",
        'k',
    )]);
    kobload_api::repositories::app_settings::set(
        &rig.tv.server.pool,
        kobload_api::repositories::app_settings::keys::DEBRID_LOGGING_ENABLED,
        &json!(true),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE debrid_providers SET provider_type = 'real_debrid' WHERE id = $1")
        .bind(rig.provider)
        .execute(&rig.tv.server.pool)
        .await
        .unwrap();

    let (status, body) = rig
        .post(
            "/api/v1/stream/resolve",
            json!({ "debrid_provider_id": rig.provider, "release_key": h.to_uppercase() }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let resolved: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(resolved["release_key"], h.as_str());

    let resp = rig
        .tv
        .server
        .admin_client()
        .get("/api/v1/stream-attempts")
        .await;
    resp.assert_ok();
    let attempts: Vec<Value> = resp.json();
    assert_eq!(attempts.len(), 1, "{attempts:?}");
    assert_eq!(attempts[0]["release_key"], h.as_str());
    assert!(attempts[0].get("info_hash").is_none());

    let cached: Option<bool> = sqlx::query_scalar(
        "SELECT available FROM debrid_availability_cache WHERE release_key = $1",
    )
    .bind(&h)
    .fetch_optional(&rig.tv.server.pool)
    .await
    .unwrap();
    assert_eq!(cached, Some(true));

    rig.report(&h, true).await;
    let options = rig.options().await;
    assert_eq!(options[0].release_key.as_deref(), Some(h.as_str()));
    assert_eq!(options[0].playback, Some(StreamPlayback::Worked));
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_two_indexers_listing_one_imported_nzb_share_its_outcomes() {
    let rig = Rig::start("Alias Merge Show", Vec::new()).await;
    let other_indexer = Uuid::new_v4();
    rig.tv.indexer.set_search_results(vec![
        rig.nzb("g-a", "Alias.Merge.Show.S01E05.1080p.WEB-DL.x264-ONE", None),
        rig.nzb(
            "g-b",
            "Alias.Merge.Show.S01E05.1080p.WEB-DL.x264-TWO",
            Some(other_indexer),
        ),
    ]);
    let pool = &rig.tv.server.pool;
    let first = rig.options().await;
    assert!(usenet(&first).iter().all(|o| o.playback.is_none()));

    let key = "nzb-5e7a";
    let source = rig.tv.source.id.to_string();
    assert!(
        usenet_releases::set_usenet_release_key(pool, &source, "g-a", key)
            .await
            .unwrap()
    );
    assert!(
        usenet_releases::set_usenet_release_key(pool, &other_indexer.to_string(), "g-b", key)
            .await
            .unwrap()
    );
    assert!(
        !usenet_releases::set_usenet_release_key(pool, "nobody", "unlisted", key)
            .await
            .unwrap()
    );
    let mut aliases = usenet_releases::aliases(pool, key).await.unwrap();
    aliases.sort_by(|a, b| a.guid.cmp(&b.guid));
    assert_eq!(
        aliases,
        vec![
            NzbRef {
                indexer_id: source.clone(),
                guid: "g-a".to_string()
            },
            NzbRef {
                indexer_id: other_indexer.to_string(),
                guid: "g-b".to_string()
            },
        ]
    );

    rig.report(key, false).await;
    let options = rig.options().await;
    let nzb = usenet(&options);
    assert_eq!(nzb.len(), 2, "{options:?}");
    for option in nzb {
        assert_eq!(option.release_key.as_deref(), Some(key));
        assert_eq!(
            option.playback,
            Some(StreamPlayback::Failed),
            "{}",
            option.release_title
        );
    }
    rig.tv.server.cleanup().await;
}
