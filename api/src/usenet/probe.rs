use std::sync::Arc;
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, Lines,
};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

const TIMEOUT: Duration = Duration::from_secs(15);

pub struct ProbeTarget<'a> {
    pub host: &'a str,
    pub port: u16,
    pub tls: bool,
    pub verify_cert: bool,
    pub username: &'a str,
    pub password: &'a str,
}

// The reply reaches the admin UI, so it never echoes credentials.
pub async fn probe(target: &ProbeTarget<'_>) -> Result<String, String> {
    tokio::time::timeout(TIMEOUT, run(target))
        .await
        .unwrap_or_else(|_| {
            Err(format!(
                "No answer from {}:{} within {}s",
                target.host,
                target.port,
                TIMEOUT.as_secs()
            ))
        })
}

async fn run(target: &ProbeTarget<'_>) -> Result<String, String> {
    let tcp = TcpStream::connect((target.host, target.port))
        .await
        .map_err(|e| format!("Could not connect to {}:{}: {e}", target.host, target.port))?;
    if !target.tls {
        return converse(tcp, target).await;
    }
    let name = ServerName::try_from(target.host.to_string())
        .map_err(|_| format!("{} is not a valid TLS server name", target.host))?;
    let stream = TlsConnector::from(Arc::new(tls_config(target.verify_cert)?))
        .connect(name, tcp)
        .await
        .map_err(|e| format!("TLS handshake failed: {e}"))?;
    converse(stream, target).await
}

async fn converse<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    target: &ProbeTarget<'_>,
) -> Result<String, String> {
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read).lines();
    let greeting = reply(&mut lines).await?;
    if !greeting.starts_with("200") && !greeting.starts_with("201") {
        return Err(format!("Unexpected greeting: {greeting}"));
    }
    send(&mut write, &format!("AUTHINFO USER {}", target.username)).await?;
    let mut login = reply(&mut lines).await?;
    if login.starts_with("381") {
        send(&mut write, &format!("AUTHINFO PASS {}", target.password)).await?;
        login = reply(&mut lines).await?;
    }
    send(&mut write, "QUIT").await.ok();
    if login.starts_with("281") {
        Ok(format!(
            "Logged in: {}",
            greeting.get(4..).unwrap_or_default().trim()
        ))
    } else {
        Err(format!("Login refused: {}", login.trim()))
    }
}

async fn reply<R: AsyncBufRead + Unpin>(lines: &mut Lines<R>) -> Result<String, String> {
    lines
        .next_line()
        .await
        .map_err(|e| format!("Connection lost: {e}"))?
        .ok_or_else(|| "The server closed the connection".to_string())
}

async fn send<W: AsyncWrite + Unpin>(write: &mut W, line: &str) -> Result<(), String> {
    write
        .write_all(format!("{line}\r\n").as_bytes())
        .await
        .map_err(|e| format!("Connection lost: {e}"))
}

fn tls_config(verify_cert: bool) -> Result<ClientConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?;
    let config = if verify_cert {
        let roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        builder.with_root_certificates(roots).with_no_client_auth()
    } else {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
            .with_no_client_auth()
    };
    Ok(config)
}

#[derive(Debug)]
struct AnyCertificate(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
