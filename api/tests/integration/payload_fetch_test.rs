//! Release payloads come from URLs an indexer chose, so every hop of the fetch is judged.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use kobload_api::payload_fetch::{PayloadClients, PayloadError, PayloadGet};
use kobload_api::redirect_guard::Origin;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use tokio::net::TcpListener;

const KEY: (&str, &str) = ("X-Api-Key", "indexer-header-key");

#[derive(Default)]
struct Host {
    next: Mutex<Option<String>>,
    seen: Mutex<Vec<(String, Option<String>)>>,
}

async fn hop(State(host): State<Arc<Host>>, Path(p): Path<String>, headers: HeaderMap) -> Response {
    let key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    host.seen.lock().unwrap().push((p.clone(), key));
    match (p.as_str(), host.next.lock().unwrap().clone()) {
        ("start", Some(next)) => (StatusCode::FOUND, [(header::LOCATION, next)]).into_response(),
        _ => "payload".into_response(),
    }
}

async fn serve() -> (SocketAddr, Arc<Host>) {
    let host = Arc::new(Host::default());
    let app = axum::Router::new()
        .route("/{p}", axum::routing::get(hop))
        .with_state(Arc::clone(&host));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.ok() });
    (addr, host)
}

fn clients(names: &[(&'static str, SocketAddr)]) -> PayloadClients {
    let names = names.to_vec();
    PayloadClients::build(move |builder| {
        names
            .iter()
            .fold(builder, |b, (name, addr)| b.resolve(name, *addr))
    })
}

#[tokio::test]
async fn a_public_indexer_cannot_bounce_a_fetch_onto_the_lan() {
    let (public, host) = serve().await;
    let (lan, lan_host) = serve().await;
    *host.next.lock().unwrap() = Some(format!("http://{lan}/secret"));
    let url = format!("http://indexer.example:{}/start", public.port());
    let got = clients(&[("indexer.example", public)])
        .get(PayloadGet {
            url: &url,
            origin: Origin::indexer(Some(&format!("http://indexer.example:{}", public.port()))),
            ..Default::default()
        })
        .await;
    assert!(matches!(got, Err(PayloadError::Refused(_))), "{got:?}");
    assert!(lan_host.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn lan_to_public_to_lan_is_refused() {
    let (lan, lan_host) = serve().await;
    let (public, public_host) = serve().await;
    let (inner, inner_host) = serve().await;
    *lan_host.next.lock().unwrap() = Some(format!("http://cdn.example:{}/start", public.port()));
    *public_host.next.lock().unwrap() = Some(format!("http://{inner}/secret"));
    let got = clients(&[("cdn.example", public)])
        .get(PayloadGet {
            url: &format!("http://{lan}/start"),
            origin: Origin::indexer(Some(&format!("http://{lan}"))),
            ..Default::default()
        })
        .await;
    assert!(matches!(got, Err(PayloadError::Refused(_))), "{got:?}");
    assert!(inner_host.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_public_indexer_cannot_hand_over_a_lan_url_directly() {
    let (lan, lan_host) = serve().await;
    let got = PayloadClients::shared()
        .get(PayloadGet {
            url: &format!("http://{lan}/secret"),
            origin: Origin::indexer(Some("https://indexer.example")),
            ..Default::default()
        })
        .await;
    assert!(matches!(got, Err(PayloadError::Refused(_))), "{got:?}");
    assert!(lan_host.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_api_key_stays_on_the_indexers_host() {
    let (indexer, host) = serve().await;
    let (cdn, cdn_host) = serve().await;
    *host.next.lock().unwrap() = Some(format!("http://cdn.example:{}/file", cdn.port()));
    let auth = (KEY.0.to_string(), KEY.1.to_string());
    let response = clients(&[("indexer.example", indexer), ("cdn.example", cdn)])
        .get(PayloadGet {
            url: &format!("http://indexer.example:{}/start", indexer.port()),
            origin: Origin::indexer(Some(&format!("http://indexer.example:{}", indexer.port()))),
            auth_header: Some(&auth),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "payload");
    assert_eq!(host.seen.lock().unwrap()[0].1.as_deref(), Some(KEY.1));
    assert_eq!(cdn_host.seen.lock().unwrap()[0].1, None);
}

#[tokio::test]
async fn a_lan_indexer_keeps_working_through_its_own_redirects() {
    let (lan, host) = serve().await;
    let (other, other_host) = serve().await;
    *host.next.lock().unwrap() = Some(format!("http://{other}/file"));
    let response = PayloadClients::shared()
        .get(PayloadGet {
            url: &format!("http://{lan}/start"),
            origin: Origin::indexer(Some(&format!("http://{lan}"))),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "payload");
    assert_eq!(other_host.seen.lock().unwrap().len(), 1);
}

struct FixedDns(Vec<IpAddr>);

impl Resolve for FixedDns {
    fn resolve(&self, _: Name) -> Resolving {
        let addrs: Vec<SocketAddr> = self.0.iter().map(|ip| SocketAddr::new(*ip, 0)).collect();
        Box::pin(async move { Ok(Box::new(addrs.into_iter()) as Addrs) })
    }
}

fn answering(ips: &[&str]) -> PayloadClients {
    let ips = ips.iter().map(|ip| ip.parse().unwrap()).collect();
    PayloadClients::with_dns(Arc::new(FixedDns(ips)), |b| b)
}

#[tokio::test]
async fn dns_answering_private_does_not_make_an_indexer_lan() {
    let (lan, lan_host) = serve().await;
    let got = PayloadClients::shared()
        .get(PayloadGet {
            url: &format!("http://{lan}/secret"),
            origin: Origin::indexer(Some("http://localhost.localdomain:9696")),
            ..Default::default()
        })
        .await;
    assert!(matches!(got, Err(PayloadError::Refused(_))), "{got:?}");
    assert!(lan_host.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_public_indexers_own_name_cannot_rebind_onto_the_lan() {
    let (lan, lan_host) = serve().await;
    for answer in [&["127.0.0.1"][..], &["127.0.0.1", "93.184.216.34"][..]] {
        let origin = format!("http://rebind.example:{}", lan.port());
        let got = answering(answer)
            .get(PayloadGet {
                url: &format!("{origin}/start"),
                origin: Origin::indexer(Some(&origin)),
                ..Default::default()
            })
            .await;
        assert!(got.is_err(), "{answer:?}: {got:?}");
        assert!(lan_host.seen.lock().unwrap().is_empty(), "{answer:?}");
    }
}

#[tokio::test]
async fn a_lan_indexers_redirect_to_a_public_name_cannot_rebind_onto_the_lan() {
    let (lan, host) = serve().await;
    let (inner, inner_host) = serve().await;
    *host.next.lock().unwrap() = Some(format!("http://rebind.example:{}/secret", inner.port()));
    let got = answering(&["127.0.0.1"])
        .get(PayloadGet {
            url: &format!("http://{lan}/start"),
            origin: Origin::indexer(Some(&format!("http://{lan}"))),
            ..Default::default()
        })
        .await;
    assert!(got.is_err(), "{got:?}");
    assert!(inner_host.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_proxy_cannot_resolve_a_public_name_past_the_guard() {
    let (proxy, proxy_host) = serve().await;
    let clients = PayloadClients::with_dns(
        Arc::new(FixedDns(vec!["127.0.0.1".parse().unwrap()])),
        move |b| b.proxy(reqwest::Proxy::all(format!("http://{proxy}")).unwrap()),
    );
    let got = clients
        .get(PayloadGet {
            url: "http://rebind.example/secret",
            origin: Origin::Indexer(None),
            ..Default::default()
        })
        .await;
    assert!(got.is_err(), "{got:?}");
    assert!(proxy_host.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_indexer_configured_by_internal_name_keeps_working() {
    let (lan, host) = serve().await;
    for name in ["prowlarr", "nas.local", "nas.lan", "nas.internal"] {
        let origin = format!("http://{name}:{}", lan.port());
        let response = answering(&["127.0.0.1"])
            .get(PayloadGet {
                url: &format!("{origin}/file"),
                origin: Origin::indexer(Some(&origin)),
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(response.text().await.unwrap(), "payload", "{name}");
    }
    assert_eq!(host.seen.lock().unwrap().len(), 4);
}
