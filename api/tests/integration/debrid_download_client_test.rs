use crate::common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::TestServer;
use common::debrid_mock::MockDebrid;
use common::stream_fixtures::hash;
use kobload_api::download_clients::DownloadClientHandler;
use kobload_api::download_clients::debrid::DebridHandler;
use kobload_api::services::stream::{StreamResolveContext, stream_resolve};
use kobload_api::types::DownloadClientConfig;
use nats_jobs::types::StreamResolveRequest;
use uuid::Uuid;

struct Fixture {
    server: TestServer,
    mock: Arc<MockDebrid>,
    handler: Arc<DebridHandler>,
    config: Arc<DownloadClientConfig>,
}

impl Fixture {
    async fn new() -> Self {
        let server = TestServer::start().await;
        let provider_id: Uuid = sqlx::query_scalar(
            "INSERT INTO debrid_providers (name, provider_type, api_key)
             VALUES ('mock', 'real_debrid', $1) RETURNING id",
        )
        .bind(Uuid::new_v4().to_string())
        .fetch_one(&server.pool)
        .await
        .expect("create provider");
        let mock = MockDebrid::new(Duration::ZERO);
        let handler = Arc::new(DebridHandler::new(
            server.pool.clone(),
            crate::common::local_only_http_client(),
            mock.resolver(),
        ));
        let now = time::OffsetDateTime::now_utc();
        let config = Arc::new(DownloadClientConfig {
            id: Uuid::new_v4(),
            name: "debrid".to_string(),
            client_type: "debrid".to_string(),
            protocol: "torrent".to_string(),
            base_url: String::new(),
            api_key: Some(provider_id.to_string()),
            username: None,
            password: None,
            category: None,
            download_dir: Some("/tmp/kobload-debrid-test".to_string()),
            is_default: false,
            delete_after_import: false,
            seed_ratio: None,
            seed_time_minutes: None,
            enabled: true,
            created_at: now,
            updated_at: now,
        });
        Self {
            server,
            mock,
            handler,
            config,
        }
    }

    async fn grab(&self, h: &str) -> String {
        self.handler
            .add_url(
                &crate::common::local_only_http_client(),
                &self.config,
                &format!("magnet:?xt=urn:btih:{h}"),
            )
            .await
            .expect("add_url")
    }
}

#[tokio::test]
async fn test_download_grabs_share_the_account_limiter() {
    let f = Fixture::new().await;
    let started = Instant::now();
    let grabs: Vec<_> = ['1', '2', '3', '4']
        .into_iter()
        .map(|c| {
            let handler = f.handler.clone();
            let config = f.config.clone();
            tokio::spawn(async move {
                handler
                    .add_url(
                        &crate::common::local_only_http_client(),
                        &config,
                        &format!("magnet:?xt=urn:btih:{}", hash(c)),
                    )
                    .await
                    .expect("add_url")
            })
        })
        .collect();
    for g in grabs {
        g.await.unwrap();
    }
    let calls = f.mock.calls();
    let early = calls
        .iter()
        .filter(|c| c.started < started + Duration::from_millis(300))
        .count();
    assert!(
        early <= 5,
        "a burst of grabs must leave the user's reserve untouched, got {early} calls at once"
    );
    assert_eq!(
        f.mock.count("list_torrents", None),
        1,
        "grabs share the account listing"
    );
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_download_grab_backs_off_on_429() {
    let f = Fixture::new().await;
    f.mock.rate_limit_next_adds(1);
    f.grab(&hash('a')).await;
    let adds: Vec<Instant> = f
        .mock
        .calls()
        .into_iter()
        .filter(|c| c.op == "add_magnet")
        .map(|c| c.started)
        .collect();
    assert_eq!(adds.len(), 2);
    assert!(adds[1] - adds[0] >= f.mock.retry_after);
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_download_grab_reuses_a_torrent_a_stream_already_added() {
    let f = Fixture::new().await;
    let resolver = f.mock.resolver();
    let ctx = StreamResolveContext {
        pool: &f.server.pool,
        http_client: &f.server.app_state.http_client,
        debrid_providers: resolver.as_ref(),
    };
    let request = StreamResolveRequest {
        debrid_provider_id: Uuid::parse_str(f.config.api_key.as_deref().unwrap()).unwrap(),
        info_hash: Some(hash('a')),
        magnet_url: None,
        nzb_url: None,
        file_id: None,
        priority: None,
    };
    assert!(stream_resolve(&ctx, &request).await.unwrap().cached);
    f.grab(&hash('a')).await;
    assert_eq!(f.mock.count("add_magnet", None), 1);
    assert_eq!(f.mock.count("list_torrents", None), 1);
    assert_eq!(f.mock.torrents_for(&hash('a')), 1);
    f.server.cleanup().await;
}
