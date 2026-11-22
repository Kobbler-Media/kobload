use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use http_body_util::BodyExt;
use kob_usenet::vendor::testkit::{Article, LoopbackServer, post};
use kob_usenet::{Engine, EngineConfig, IpPreference, ServerConfig};
use kobload_usenet::{AppState, router};
use tower::ServiceExt;
use usenet_proto::{ImportMode, ImportRequest, ImportResponse, Purpose, StreamClaims};

pub const ARTICLE: usize = 16 * 1024;
pub const CONTROL: &str = "control-key";
pub const METRICS: &str = "metrics-key";
pub const SECRET: &[u8] = b"stream-secret";
const POSTED: i64 = 1_790_000_000;
const EBML: [u8; 4] = [0x1A, 0x45, 0xDF, 0xA3];

pub struct Posted {
    pub name: &'static str,
    pub data: Vec<u8>,
    pub articles: Vec<Article>,
}

pub struct Fixture {
    pub files: Vec<Posted>,
}

fn posted(name: &'static str, prefix: &str, len: usize, seed: u8) -> Posted {
    let mut data: Vec<u8> = (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed) | 1)
        .collect();
    data[..4].copy_from_slice(&EBML);
    let articles = post(prefix, name, &data, ARTICLE);
    Posted {
        name,
        data,
        articles,
    }
}

impl Fixture {
    pub fn season() -> Self {
        Self {
            files: vec![
                posted("Show.S01E01.mkv", "e01", 12 * ARTICLE + 777, 3),
                posted("Show.S01E02.mkv", "e02", 9 * ARTICLE + 1234, 7),
            ],
        }
    }

    pub fn feature() -> Self {
        Self {
            files: vec![posted("Feature.2026.mkv", "f", 48 * ARTICLE + 99, 11)],
        }
    }

    pub fn articles(&self) -> Vec<Article> {
        self.files
            .iter()
            .flat_map(|f| f.articles.iter().cloned())
            .collect()
    }

    pub fn without(&self, file: usize, article: usize) -> Vec<Article> {
        let gone = &self.files[file].articles[article].message_id;
        self.articles()
            .into_iter()
            .filter(|a| &a.message_id != gone)
            .collect()
    }

    pub fn message_id(&self, file: usize, article: usize) -> &str {
        &self.files[file].articles[article].message_id
    }

    pub fn data(&self, file: usize) -> &[u8] {
        &self.files[file].data
    }

    pub fn nzb(&self) -> Vec<u8> {
        self.padded_nzb(0)
    }

    pub fn padded_nzb(&self, padding: usize) -> Vec<u8> {
        let mut xml = String::from(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\">\n",
        );
        let total = self.files.len();
        for (n, file) in self.files.iter().enumerate() {
            xml.push_str(&format!(
                "<file poster=\"p\" date=\"{POSTED}\" subject=\"[{}/{total}] - &quot;{}&quot; yEnc (1/1)\"><groups><group>a.b.test</group></groups><segments>\n",
                n + 1,
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
        xml.push_str(&" ".repeat(padding));
        xml.push_str("</nzb>\n");
        xml.into_bytes()
    }
}

pub fn server(id: &str, nntp: &LoopbackServer) -> ServerConfig {
    ServerConfig {
        id: id.to_owned(),
        name: format!("server {id}"),
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
    }
}

pub async fn engine(dir: &Path, servers: Vec<ServerConfig>) -> Arc<Engine> {
    warm_engine(dir, servers, 0).await
}

pub async fn warm_engine(
    dir: &Path,
    servers: Vec<ServerConfig>,
    min_connections: usize,
) -> Arc<Engine> {
    let mut config = EngineConfig::new(dir.to_path_buf(), servers);
    config.min_connections = min_connections;
    Arc::new(Engine::new(config).await.expect("engine"))
}

pub struct Harness {
    pub app: Router,
}

impl Harness {
    pub async fn new(dir: &Path, nntp: &LoopbackServer) -> Self {
        Self::with_gap(dir, nntp, kobload_usenet::BYTE_GAP).await
    }

    pub async fn with_gap(dir: &Path, nntp: &LoopbackServer, gap: Duration) -> Self {
        let engine = engine(dir, vec![server("a", nntp)]).await;
        let state = AppState::new(engine, CONTROL, SECRET)
            .with_byte_gap(gap)
            .with_metrics_key(Some(METRICS));
        state.configured().set();
        Self { app: router(state) }
    }

    pub async fn send(&self, request: Request<Body>) -> Response<Body> {
        self.app.clone().oneshot(request).await.expect("infallible")
    }

    pub async fn control(
        &self,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> Response<Body> {
        let builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {CONTROL}"));
        let request = match body {
            Some(json) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json.to_string())),
            None => builder.body(Body::empty()),
        };
        self.send(request.unwrap()).await
    }

    pub async fn import(&self, fixture: &Fixture, cache_key: &str) -> ImportResponse {
        let response = self
            .control(
                "POST",
                "/v1/import",
                Some(import_body(Some(fixture), cache_key)),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        json(response).await
    }

    pub async fn stream(&self, method: &str, token: &str, range: Option<&str>) -> Response<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(format!("/stream/{token}"));
        if let Some(range) = range {
            builder = builder.header(header::RANGE, range);
        }
        self.send(builder.body(Body::empty()).unwrap()).await
    }

    pub async fn metrics(&self) -> String {
        let request = Request::get("/metrics")
            .header(header::AUTHORIZATION, format!("Bearer {METRICS}"))
            .body(Body::empty())
            .unwrap();
        let response = self.send(request).await;
        assert_eq!(response.status(), StatusCode::OK);
        String::from_utf8(collect(response).await.0).unwrap()
    }
}

pub fn import_body(fixture: Option<&Fixture>, cache_key: &str) -> serde_json::Value {
    serde_json::to_value(ImportRequest {
        cache_key: cache_key.to_owned(),
        name: "Show.S01".to_owned(),
        mode: ImportMode::Stream,
        password: None,
        nzb_base64: fixture.map(|f| STANDARD.encode(f.nzb())),
    })
    .unwrap()
}

pub fn token(release_key: &str, output: usize, purpose: Purpose) -> String {
    signed(SECRET, release_key, output, purpose, now() + 3600)
}

pub fn signed(
    secret: &[u8],
    release_key: &str,
    output: usize,
    purpose: Purpose,
    expires_at: i64,
) -> String {
    usenet_proto::sign(
        secret,
        &StreamClaims {
            release_key: release_key.to_owned(),
            output,
            purpose,
            expires_at,
        },
    )
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

pub fn output(release: &ImportResponse, path: &str) -> usize {
    release
        .outputs
        .iter()
        .find(|o| o.path == path)
        .expect("output")
        .index
}

pub async fn json<T: serde::de::DeserializeOwned>(response: Response<Body>) -> T {
    serde_json::from_slice(&collect(response).await.0).expect("json body")
}

pub async fn collect(response: Response<Body>) -> (Vec<u8>, Option<String>) {
    let mut body = response.into_body();
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        match frame {
            Ok(frame) => {
                bytes.extend_from_slice(&frame.into_data().unwrap_or_else(|_| Bytes::new()))
            }
            Err(error) => return (bytes, Some(error.to_string())),
        }
    }
    (bytes, None)
}

pub fn metric(text: &str, name: &str, labels: &[&str]) -> u64 {
    text.lines()
        .filter(|l| l.starts_with(&format!("{name}{{")))
        .filter(|l| labels.iter().all(|label| l.contains(label)))
        .map(|l| l.rsplit(' ').next().unwrap().parse::<u64>().unwrap())
        .sum()
}
