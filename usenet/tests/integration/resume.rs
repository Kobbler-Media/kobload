use std::sync::Arc;
use std::time::Duration;

use kob_usenet::vendor::testkit::LoopbackServer;
use kob_usenet::{Engine, EngineConfig, JobSpec, JobState};
use kobload_usenet::{Booted, ConfigPull, Configured, start_engine};
use usenet_proto::{EngineConfigResponse, IpPreference, UsenetServer};

use crate::support::{CONTROL, Fixture, quick_retries, server};

fn stored(nntp: &LoopbackServer) -> EngineConfigResponse {
    EngineConfigResponse {
        servers: vec![UsenetServer {
            id: "a".to_owned(),
            name: "server a".to_owned(),
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
        }],
    }
}

// The pause stands for whatever runs between the engine starting and the pull adopting its
// stored config; resumed jobs must not depend on it being short.
async fn restart(dir: &std::path::Path) -> (Arc<Engine>, Configured) {
    let mut config = EngineConfig::new(dir.to_path_buf(), Vec::new());
    config.download = quick_retries(1);
    let Booted { engine, restored } = start_engine(config).await.unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let configured = Configured::default();
    let mut pull = ConfigPull::new(
        Arc::clone(&engine),
        "http://127.0.0.1:9",
        CONTROL,
        dir,
        configured.clone(),
    );
    pull.adopt(restored.expect("a stored config"));
    (engine, configured)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_job_resumed_at_restart_starts_with_the_stored_servers() {
    let fixture = Fixture::feature();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.json"),
        serde_json::to_vec(&stored(&nntp)).unwrap(),
    )
    .unwrap();
    let (arrived, release) = nntp.hold_article(fixture.message_id(0, 20));
    {
        let mut config = EngineConfig::new(dir.path().to_path_buf(), vec![server("a", &nntp)]);
        config.download = quick_retries(1);
        let engine = Engine::new(config).await.unwrap();
        engine
            .add_job(JobSpec {
                id: "dl-resume".to_owned(),
                name: "Feature.2026.1080p".to_owned(),
                category: "kobload".to_owned(),
                nzb: fixture.nzb(),
                password: None,
                dest_dir: dir.path().join("downloads"),
            })
            .await
            .unwrap();
        arrived.await.unwrap();
    }
    drop(release);
    while nntp.open_connections() > 0 {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let (engine, configured) = restart(dir.path()).await;
    assert!(configured.is_set());
    let status = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let status = engine.job("dl-resume").unwrap();
            if matches!(status.state, JobState::Completed | JobState::Failed) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the resumed job settles");
    assert_eq!(status.state, JobState::Completed, "{status:?}");
}
