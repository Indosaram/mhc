//! Loopback TLS fixture; the client uses only a synthetic trust root and DNS resolver.

use rustls_pki_types::PrivateKeyDer;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::{TlsAcceptor, rustls};

const COMPLETED: &str = "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"fixture\",\"status\":\"completed\",\"output\":[]}}\n\n";

#[derive(Debug, PartialEq, Eq)]
pub(super) struct WireRequest {
    pub(super) line: String,
    pub(super) headers: std::collections::BTreeMap<String, String>,
    pub(super) body: Vec<u8>,
}

pub(super) async fn wire_client(
    host: &str,
    count: usize,
) -> (reqwest::Client, tokio::task::JoinHandle<Vec<WireRequest>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let key = rcgen::KeyPair::generate().expect("ephemeral fixture key");
    let cert = rcgen::CertificateParams::new(vec![host.into()])
        .expect("fixture params")
        .self_signed(&key)
        .expect("loopback cert");
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("TLS versions")
    .with_no_client_auth()
    .with_single_cert(
        vec![cert.der().clone()],
        PrivateKeyDer::Pkcs8(key.serialize_der().into()),
    )
    .expect("fixture server TLS");
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let address = listener.local_addr().expect("loopback address");
    let client = reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_der(cert.der()).expect("fixture root"))
        // Route DNS to the local listener; TLS accepts only the fixture's original hostname.
        .dns_resolver(Arc::new(LoopbackDns(address)))
        .no_proxy()
        .http1_only()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("fixture client");
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for _ in 0..count {
            let (socket, _) = listener.accept().await.expect("fixture connection");
            let mut stream = acceptor.accept(socket).await.expect("fixture TLS");
            let mut bytes = Vec::new();
            let header_end = loop {
                let count = stream.read_buf(&mut bytes).await.expect("headers");
                assert_ne!(count, 0);
                if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let mut lines = std::str::from_utf8(&bytes[..header_end])
                .expect("header text")
                .lines();
            let line = lines.next().expect("request line").to_owned();
            let headers: std::collections::BTreeMap<_, _> = lines
                .filter_map(|line| line.split_once(':'))
                .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
                .collect();
            let length = headers["content-length"]
                .parse::<usize>()
                .expect("content-length");
            let mut body = bytes[header_end..].to_vec();
            while body.len() < length {
                assert_ne!(stream.read_buf(&mut body).await.expect("request body"), 0);
            }
            requests.push(WireRequest {
                line,
                headers,
                body,
            });
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{COMPLETED}",
                COMPLETED.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("fixture response");
            stream.shutdown().await.expect("close fixture response");
        }
        requests
    });
    (client, task)
}

struct LoopbackDns(std::net::SocketAddr);

impl reqwest::dns::Resolve for LoopbackDns {
    fn resolve(&self, _: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let address = self.0;
        Box::pin(async move { Ok(Box::new(std::iter::once(address)) as reqwest::dns::Addrs) })
    }
}
