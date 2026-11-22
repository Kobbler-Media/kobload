//! kobload-api's engine client and `usenet` download client against the real
//! `kobload-usenet` router and a loopback NNTP server, so a drift in paths, query encoding,
//! status codes or error mapping between the two shows here rather than in production.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use kob_usenet::vendor::testkit::{Article, LoopbackServer, post};
use kob_usenet::{Engine, EngineConfig, IpPreference, ServerConfig};
use kobload_api::debrid::DefaultDebridProviderResolver;
use kobload_api::download_clients::{
    DefaultDownloadClientResolver, DownloadClientHandler, DownloadClientResolver, NewDownload,
};
use kobload_api::types::DownloadClientConfig;
use kobload_api::usenet::{EngineError, UsenetEngine};
use usenet_proto::{ImportMode, ImportRequest, Purpose};
use uuid::Uuid;

use crate::common::server::local_only_http_client;
use crate::common::usenet::{CONTROL_KEY, STREAM_SECRET};
use crate::common::usenet_jobs::engine_settings;

const ARTICLE: usize = 16 * 1024;

struct Posted {
    name: &'static str,
    data: Vec<u8>,
    articles: Vec<Article>,
}

fn posted(name: &'static str, prefix: &str, len: usize, seed: u8) -> Posted {
    let mut data: Vec<u8> = (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed) | 1)
        .collect();
    data[..4].copy_from_slice(&[0x1A, 0x45, 0xDF, 0xA3]);
    let articles = post(prefix, name, &data, ARTICLE);
    Posted {
        name,
        data,
        articles,
    }
}

fn nzb(files: &[Posted]) -> Vec<u8> {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\">\n",
    );
    for (n, file) in files.iter().enumerate() {
        xml.push_str(&format!(
            "<file poster=\"p\" date=\"1790000000\" subject=\"[{}/{}] - &quot;{}&quot; yEnc (1/1)\"><groups><group>a.b.test</group></groups><segments>\n",
            n + 1,
            files.len(),
            file.name
        ));
        for (part, article) in file.articles.iter().enumerate() {
            xml.push_str(&format!(
                "<segment bytes=\"{}\" number=\"{}\">{}</segment>\n",
                article.wire.len(),
                part + 1,
                article.message_id
            ));
        }
        xml.push_str("</segments></file>\n");
    }
    xml.push_str("</nzb>\n");
    xml.into_bytes()
}

struct Rig {
    engine: Arc<UsenetEngine>,
    handler: Arc<dyn DownloadClientHandler>,
    url: String,
    _nntp: LoopbackServer,
    _dir: tempfile::TempDir,
}

async fn rig(articles: Vec<Article>) -> Rig {
    let nntp = LoopbackServer::start(articles).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let server = ServerConfig {
        id: "a".to_owned(),
        name: "loopback".to_owned(),
        host: "127.0.0.1".to_owned(),
        port: nntp.addr().port(),
        tls: false,
        verify_cert: true,
        ip_preference: IpPreference::Any,
        connections: 4,
        priority: 0,
        backbone: None,
        retention_days: None,
        expires_on: None,
        quota_bytes: None,
        username: "user".to_owned(),
        password: "hunter2-secret".to_owned(),
    };
    let mut config = EngineConfig::new(dir.path().join("engine"), vec![server]);
    config.download.free_space_floor = 1 << 20;
    let engine = Arc::new(Engine::new(config).await.unwrap());
    let state = kobload_usenet::AppState::new(engine, CONTROL_KEY, STREAM_SECRET.as_bytes())
        .with_download_dir(&dir.path().join("downloads"));
    state.configured().set();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = kobload_usenet::router(state);
    tokio::spawn(async move { axum::serve(listener, app).await.ok() });

    let mut settings = engine_settings(&url);
    settings.stream_base_url = url.clone();
    let engine = Arc::new(UsenetEngine::new(settings, local_only_http_client()));
    let resolver = DefaultDownloadClientResolver::new(
        sqlx::PgPool::connect_lazy("postgres://unused").unwrap(),
        local_only_http_client(),
        Arc::new(DefaultDebridProviderResolver::with_usenet(Arc::clone(
            &engine,
        ))),
    );
    Rig {
        engine,
        handler: resolver.get("usenet").expect("usenet handler"),
        url,
        _nntp: nntp,
        _dir: dir,
    }
}

fn client() -> DownloadClientConfig {
    let now = time::OffsetDateTime::now_utc();
    DownloadClientConfig {
        id: Uuid::nil(),
        name: "Usenet".to_string(),
        client_type: "usenet".to_string(),
        protocol: "usenet".to_string(),
        base_url: String::new(),
        api_key: None,
        username: None,
        password: None,
        category: Some("kobload".to_string()),
        download_dir: None,
        is_default: true,
        delete_after_import: true,
        seed_ratio: None,
        seed_time_minutes: None,
        enabled: true,
        created_at: now,
        updated_at: now,
    }
}

fn import_request(cache_key: &str, nzb: Option<&[u8]>) -> ImportRequest {
    ImportRequest {
        cache_key: cache_key.to_string(),
        name: "Show.S01.1080p.WEB-DL-GRP".to_string(),
        mode: ImportMode::Stream,
        password: None,
        nzb_base64: nzb.map(|n| base64::engine::general_purpose::STANDARD.encode(n)),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_api_client_imports_lists_samples_and_streams_a_release() {
    let season = [
        posted("Show.S01E01.mkv", "e01", 12 * ARTICLE + 777, 3),
        posted("Show.S01E02.mkv", "e02", 9 * ARTICLE + 1234, 7),
    ];
    let articles = season.iter().flat_map(|f| f.articles.clone()).collect();
    let rig = rig(articles).await;

    assert!(matches!(
        rig.engine.import(&import_request("idx:season", None)).await,
        Err(EngineError::NotCached)
    ));
    let imported = rig
        .engine
        .import(&import_request("idx:season", Some(&nzb(&season))))
        .await
        .unwrap();
    assert_eq!(imported.outputs.len(), 2);
    let again = rig
        .engine
        .import(&import_request("idx:season", None))
        .await
        .unwrap();
    assert_eq!(
        again.release_key, imported.release_key,
        "imported by cache key"
    );

    let listed = rig.engine.release(&imported.release_key).await.unwrap();
    assert_eq!(listed.outputs, imported.outputs);
    let unknown = format!("nzb-{}", "0".repeat(64));
    assert!(matches!(
        rig.engine.release(&unknown).await,
        Err(EngineError::NotCached)
    ));

    let sample = rig
        .engine
        .sample_health(&imported.release_key, 5, 1)
        .await
        .unwrap();
    assert!(sample.checked > 0 && sample.missing == 0, "{sample:?}");
    assert!(matches!(
        rig.engine.sample_health(&imported.release_key, 5, 2).await,
        Err(EngineError::NotCached)
    ));

    let invalid = rig
        .engine
        .import(&import_request("idx:junk", Some(b"not an nzb")))
        .await;
    assert!(
        matches!(&invalid, Err(EngineError::Status(400, code)) if code == "invalid_nzb"),
        "{invalid:?}"
    );

    let expires = time::OffsetDateTime::now_utc().unix_timestamp() + 600;
    let url = rig
        .engine
        .stream_url(&imported.release_key, 1, Purpose::Play, expires);
    assert!(url.starts_with(&rig.url), "{url}");
    let body = reqwest::Client::new()
        .get(&url)
        .header("range", "bytes=0-99")
        .send()
        .await
        .unwrap();
    assert_eq!(body.status(), reqwest::StatusCode::PARTIAL_CONTENT);
    assert_eq!(&body.bytes().await.unwrap()[..], &season[1].data[..100]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_download_client_runs_a_job_and_maps_engine_errors() {
    let feature = [posted("Feature.2026.mkv", "f", 48 * ARTICLE + 99, 11)];
    let rig = rig(feature[0].articles.clone()).await;
    let http = local_only_http_client();
    let client = client();
    let id = Uuid::now_v7();
    let nzb = nzb(&feature);
    let download = NewDownload {
        id,
        title: "Feature.2026.1080p.WEB-DL-GRP",
        password: None,
    };

    let added = rig
        .handler
        .add_bytes(&http, &client, &nzb, &download)
        .await
        .unwrap();
    assert_eq!(added, id.to_string());
    let duplicate = rig
        .handler
        .add_bytes(&http, &client, &nzb, &download)
        .await
        .unwrap();
    assert_eq!(duplicate, added, "a 409 duplicate_job takes over the job");
    let jobs = rig.engine.jobs().await.unwrap();
    assert_eq!(jobs.len(), 1);

    let polled = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let polled = rig.handler.poll(&http, &client, &added).await.unwrap();
            if polled.status != "downloading" {
                return polled;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the job finishes");
    assert_eq!(polled.status, "completed", "{:?}", polled.error);
    assert_eq!(polled.files.len(), 1, "{:?}", polled.files);
    let storage = polled.client_data.unwrap()["storage"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(std::path::Path::new(&storage).exists());

    rig.handler
        .remove(&http, &client, &added, true)
        .await
        .unwrap();
    rig.handler
        .remove(&http, &client, &added, true)
        .await
        .expect("removing a removed job is not an error");
    assert!(!std::path::Path::new(&storage).exists());

    let lost = rig.handler.poll(&http, &client, &added).await.unwrap_err();
    assert!(
        lost.is_transient(),
        "an unknown job waits for reconcile: {lost}"
    );

    let mut rotated = engine_settings(&rig.url);
    rotated.control_key = "a-rotated-control-key-a-rotated-control-key".to_string();
    let resolver = DefaultDownloadClientResolver::new(
        sqlx::PgPool::connect_lazy("postgres://unused").unwrap(),
        local_only_http_client(),
        Arc::new(DefaultDebridProviderResolver::with_usenet(Arc::new(
            UsenetEngine::new(rotated, local_only_http_client()),
        ))),
    );
    let stale = resolver.get("usenet").unwrap();
    let refused = stale.poll(&http, &client, &added).await.unwrap_err();
    assert!(refused.is_transient(), "{refused}");
    assert!(refused.to_string().contains("401"), "{refused}");
}
