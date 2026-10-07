//! Registered execution QA driver: the service over a real HTTP/1.1 socket.
//!
//! Upstream is driven by the Workers runtime (`wrangler dev`). The native equivalent is a
//! real client against a local listener that routes each request through
//! `get_worker::route` with the injected test fakes (`crate::fakes`). This is a QA adapter,
//! not a shipped consumer, product registration, or a standalone CLI/cloud entry.
//!
//! Bounded and deterministic: the client connects with `TcpStream::connect_timeout`, the
//! listener is nonblocking so `accept` returns the already-queued connection at once (a
//! `WouldBlock` is a deterministic failure, never a hang), both streams carry read/write
//! timeouts, and there is no client thread and no join. The listener and streams close on
//! drop; no process, daemon or thread outlives a test.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use get_worker::{route, Body, Headers, Method, Request, Response};

use crate::fakes::Harness;

const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// A parsed HTTP response read off the wire.
pub struct HttpResult {
    pub status: u16,
    pub headers: Headers,
    pub body: Vec<u8>,
}

impl HttpResult {
    #[must_use]
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)
    }
}

/// Binds an ephemeral local port and serves requests through `route`.
pub struct Driver {
    harness: Harness,
    listener: TcpListener,
    addr: SocketAddr,
}

impl Driver {
    pub fn new(harness: Harness) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
        listener.set_nonblocking(true).expect("nonblocking listener");
        let addr = listener.local_addr().expect("local address");
        Driver {
            harness,
            listener,
            addr,
        }
    }

    #[must_use]
    pub fn harness(&self) -> &Harness {
        &self.harness
    }

    /// Sends one request over a real socket and serves it.
    pub fn exchange(&self, method: &str, path: &str, headers: &[(&str, &str)]) -> HttpResult {
        let mut client =
            TcpStream::connect_timeout(&self.addr, IO_TIMEOUT).expect("connect to the driver");
        client.set_read_timeout(Some(IO_TIMEOUT)).expect("read timeout");
        client.set_write_timeout(Some(IO_TIMEOUT)).expect("write timeout");
        client
            .write_all(&build_request(method, path, headers))
            .expect("write the request");
        client.flush().expect("flush the request");

        // The client connected before this accept, so the connection is already in the
        // accept queue: a nonblocking accept returns it at once, and `WouldBlock` is a
        // deterministic failure rather than a hang.
        let (mut server, _) = match self.listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                panic!("no connection was queued after a successful connect")
            }
            Err(error) => panic!("accept failed: {error}"),
        };
        server.set_read_timeout(Some(IO_TIMEOUT)).expect("read timeout");
        server.set_write_timeout(Some(IO_TIMEOUT)).expect("write timeout");

        let request = read_request(&mut server).expect("read the request");
        let ctx = self.harness.context();
        let response = match route(&request, &ctx) {
            Ok(response) => response,
            Err(error) => Response::new(500, Some(Body::Text(format!("{error}\n"))))
                .with_header("Content-Type", "text/plain; charset=utf-8"),
        };
        write_response(&mut server, &response).expect("write the response");
        server.shutdown(Shutdown::Write).expect("close the response stream");

        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).expect("read the response");
        parse_raw_response(&bytes)
    }
}

fn build_request(method: &str, path: &str, headers: &[(&str, &str)]) -> Vec<u8> {
    let mut out = format!("{method} {path} HTTP/1.1\r\nHost: get.omo.dev\r\n");
    for (name, value) in headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str("Connection: close\r\n\r\n");
    out.into_bytes()
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Request> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.trim_end().split(' ');
    let method = Method::parse(parts.next().unwrap_or(""));
    let target = parts.next().unwrap_or("/");
    let mut headers = Headers::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.append(name.trim(), value.trim());
        }
    }
    let url = if target.starts_with("http") {
        target.to_string()
    } else {
        format!("https://get.omo.dev{target}")
    };
    Ok(Request::new(method, url).with_headers(headers))
}

fn write_response(stream: &mut TcpStream, response: &Response) -> std::io::Result<()> {
    let body = response.body_bytes();
    let mut head = format!("HTTP/1.1 {} {}\r\n", response.status, reason(response.status));
    let mut has_length = false;
    for (name, value) in response.headers.iter() {
        if name.eq_ignore_ascii_case("Content-Length") {
            has_length = true;
        }
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if !has_length {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn parse_raw_response(bytes: &[u8]) -> HttpResult {
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map_or(bytes.len(), |index| index + 4);
    let head = String::from_utf8_lossy(&bytes[..split]);
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .unwrap_or("")
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    let mut headers = Headers::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.append(name.trim(), value.trim());
        }
    }
    HttpResult {
        status,
        headers,
        body: bytes[split..].to_vec(),
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        302 => "Found",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

#[cfg(test)]
mod tests {
    use super::Driver;
    use crate::fakes::{Harness, StaticFetcher};
    use get_worker::{INSTALL_SH, NPM_DIST_TAGS};

    const MIGRATION: &str = include_str!("../../migrations/0001_downloads.sql");
    const ANALYTICS_SQL: &str =
        "https://api.cloudflare.com/client/v4/accounts/account/analytics_engine/sql";

    fn mirror(h: &Harness, version: &str, assets: &[(&str, &str)]) {
        for (name, body) in assets {
            h.bucket.put(&format!("releases/v{version}/{name}"), body);
        }
        h.bucket.put(&format!("releases/v{version}/.complete"), "{}");
    }

    fn driver(fetcher: StaticFetcher) -> Driver {
        Driver::new(Harness::new(MIGRATION, fetcher))
    }

    #[test]
    fn mirrored_get_serves_the_object_and_its_headers_over_http() {
        let driver = driver(StaticFetcher::new());
        mirror(driver.harness(), "5.1.1", &[("omo-linux-x64", "binary-bytes")]);

        let response = driver.exchange("GET", "/v/5.1.1/omo-linux-x64", &[]);
        assert_eq!(response.status, 200);
        assert_eq!(response.body_text(), "binary-bytes");
        assert_eq!(response.header("Content-Type"), Some("application/octet-stream"));
        assert_eq!(response.header("Content-Length"), Some("12"));
        assert_eq!(
            response.header("Content-Disposition"),
            Some("attachment; filename=\"omo-linux-x64\"")
        );
        assert_eq!(response.header("ETag"), Some("\"releases/v5.1.1/omo-linux-x64\""));
        assert_eq!(response.header("X-Omo-Source"), Some("r2"));

        let points = driver.harness().sink.points();
        let blobs = &points.last().expect("a recorded point").blobs;
        assert_eq!(
            blobs.as_slice(),
            ["binary", "r2", "5.1.1", "omo-linux-x64", "XX", ""]
        );
    }

    #[test]
    fn head_returns_the_same_headers_with_no_body_and_records_nothing() {
        let driver = driver(StaticFetcher::new());
        mirror(driver.harness(), "5.1.1", &[("omo-linux-x64", "binary-bytes")]);

        let response = driver.exchange("HEAD", "/v/5.1.1/omo-linux-x64", &[]);
        assert_eq!(response.status, 200);
        assert!(response.body.is_empty());
        assert_eq!(response.header("Content-Length"), Some("12"));
        assert!(driver.harness().sink.points().is_empty());
        assert!(driver.harness().pending.is_empty());
    }

    #[test]
    fn a_repeat_get_is_served_from_the_edge_cache() {
        let driver = driver(StaticFetcher::new());
        mirror(driver.harness(), "5.1.1", &[("omo-darwin-arm64", "arm-bytes")]);

        let first = driver.exchange("GET", "/v/5.1.1/omo-darwin-arm64", &[]);
        assert_eq!(first.header("X-Omo-Source"), Some("r2"));
        driver.harness().settle();
        let reads = driver.harness().bucket.reads().len();

        let second = driver.exchange("GET", "/v/5.1.1/omo-darwin-arm64", &[]);
        assert_eq!(second.body_text(), "arm-bytes");
        assert_eq!(second.header("X-Omo-Source"), Some("cache"));
        assert_eq!(driver.harness().bucket.reads().len(), reads);
    }

    #[test]
    fn an_invalid_asset_is_refused_before_any_storage_read() {
        let driver = driver(StaticFetcher::new());
        mirror(driver.harness(), "5.1.1", &[("omo-linux-x64", "binary-bytes")]);

        let response = driver.exchange("GET", "/v/5.1.1/omo-plan9-x64", &[]);
        assert_eq!(response.status, 404);
        assert_eq!(response.body_text(), "unknown release asset\n");
        assert!(driver.harness().bucket.reads().is_empty());
        assert!(driver.harness().sink.points().is_empty());
    }

    #[test]
    fn a_never_mirrored_version_redirects_to_github() {
        let driver = driver(StaticFetcher::new());
        driver
            .harness()
            .bucket
            .put("releases/v5.1.0/omo-linux-x64", "half-mirrored");

        let response = driver.exchange("GET", "/v/5.1.0/omo-linux-x64", &[]);
        assert_eq!(response.status, 302);
        assert_eq!(
            response.header("Location"),
            Some("https://github.com/code-yeongyu/oh-my-openagent/releases/download/v5.1.0/omo-linux-x64")
        );
        assert_eq!(response.header("X-Omo-Fallback-Reason"), Some("version-not-mirrored"));
        assert_eq!(
            driver.harness().sink.points().last().expect("a point").blobs[1],
            "github"
        );
    }

    #[test]
    fn a_range_get_serves_the_object_and_never_populates_the_cache() {
        let driver = driver(StaticFetcher::new());
        mirror(driver.harness(), "5.1.1", &[("omo-linux-x64", "binary-bytes")]);

        let response = driver.exchange("GET", "/v/5.1.1/omo-linux-x64", &[("Range", "bytes=0-3")]);
        assert_eq!(response.status, 200);
        assert_eq!(response.body_text(), "binary-bytes");
        assert_eq!(response.header("X-Omo-Source"), Some("r2"));
        assert!(driver.harness().pending.is_empty());
    }

    #[test]
    fn a_channel_pointer_is_served_and_counted() {
        let driver = driver(StaticFetcher::new());
        driver.harness().bucket.put("channels/latest", "5.1.1\n");

        let response = driver.exchange("GET", "/channels/latest", &[]);
        assert_eq!(response.status, 200);
        assert_eq!(response.body_text(), "5.1.1\n");
        assert_eq!(response.header("Cache-Control"), Some("public, max-age=60"));
        assert_eq!(response.header("X-Omo-Source"), Some("r2"));
        let blobs = &driver.harness().sink.points().last().expect("a point").blobs;
        assert_eq!(
            blobs.as_slice(),
            ["channel", "worker", "5.1.1", "latest", "XX", ""]
        );
    }

    #[test]
    fn a_browser_root_redirects_to_the_docs_and_a_curl_root_serves_the_script() {
        let driver = driver(StaticFetcher::new());

        let browser = driver.exchange("GET", "/", &[("User-Agent", "Mozilla/5.0 Safari/605")]);
        assert_eq!(browser.status, 302);
        assert_eq!(browser.header("Location"), Some("https://omo.dev/docs/install"));

        let curl = driver.exchange("GET", "/", &[("User-Agent", "curl/8.7.1")]);
        assert_eq!(curl.status, 200);
        assert_eq!(curl.body_text(), INSTALL_SH);
    }

    #[test]
    fn health_and_method_and_unknown_routes() {
        let driver = driver(StaticFetcher::new());

        let health = driver.exchange("GET", "/healthz", &[]);
        assert_eq!(health.status, 200);
        assert_eq!(health.body_text(), "ok\n");

        let method = driver.exchange("POST", "/healthz", &[]);
        assert_eq!(method.status, 405);
        assert_eq!(method.body_text(), "method not allowed\n");
        assert_eq!(method.header("Cache-Control"), Some("no-store"));

        let unknown = driver.exchange("GET", "/nope", &[]);
        assert_eq!(unknown.status, 404);
        assert_eq!(unknown.body_text(), "not found\n");
    }

    #[test]
    fn download_stats_are_served_as_json_and_cached() {
        let driver = driver(StaticFetcher::new());
        driver.harness().db.execute(
            "INSERT INTO downloads_daily (day, kind, source, version, asset, count) VALUES ('2026-09-29', 'binary', 'r2', '5.1.1', 'omo-linux-x64', 9)",
        ).expect("seed counts");
        driver.harness().db.execute(
            "INSERT INTO downloads_daily (day, kind, source, version, asset, count) VALUES ('2026-09-29', 'binary', 'github', '5.1.1', 'omo-linux-x64', 40)",
        ).expect("seed counts");

        let response = driver.exchange("GET", "/stats/downloads", &[]);
        assert_eq!(response.status, 200);
        assert_eq!(response.header("Cache-Control"), Some("public, max-age=300"));
        assert_eq!(response.header("Access-Control-Allow-Origin"), Some("*"));
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("stats json");
        assert_eq!(body["servedFromMirror"], serde_json::json!(9));
        assert_eq!(body["redirectedToGitHub"], serde_json::json!(40));
        assert_eq!(body["uncountedByGitHub"], serde_json::json!(9));
        assert_eq!(body["rolledUpThrough"], serde_json::json!("2026-09-29"));

        driver.harness().settle();
        let cached = driver.exchange("GET", "/stats/downloads", &[]);
        assert_eq!(cached.body_text(), response.body_text());
    }

    #[test]
    fn a_channel_with_no_pointer_and_a_failing_registry_is_unavailable() {
        let driver = driver(StaticFetcher::new());

        let response = driver.exchange("GET", "/channels/beta", &[]);
        assert_eq!(response.status, 503);
        assert_eq!(response.body_text(), "channel unavailable\n");
    }

    #[test]
    fn a_channel_falls_back_to_the_npm_dist_tag_over_http() {
        let fetcher = StaticFetcher::new().with(
            NPM_DIST_TAGS,
            get_worker::FetchResponse::new(200, r#"{"latest":"5.1.1","beta":"5.2.0-0.beta.3"}"#),
        );
        let driver = driver(fetcher);

        let response = driver.exchange("GET", "/channels/beta", &[]);
        assert_eq!(response.status, 200);
        assert_eq!(response.body_text(), "5.2.0-beta.3\n");
        assert_eq!(response.header("X-Omo-Source"), Some("npm"));
    }

    #[test]
    fn a_rollup_over_http_evidence_is_stored_and_exposed() {
        let driver = driver(StaticFetcher::new().with(
            ANALYTICS_SQL,
            get_worker::FetchResponse::new(
                200,
                serde_json::json!({
                    "data": [{
                        "day": "2026-09-29",
                        "kind": "binary",
                        "source": "r2",
                        "version": "5.1.1",
                        "asset": "omo-linux-x64",
                        "count": "6"
                    }]
                })
                .to_string(),
            ),
        ));

        let stored = get_worker::run_downloads_rollup(
            driver.harness().env(),
            &StaticFetcher::new().with(
                ANALYTICS_SQL,
                get_worker::FetchResponse::new(
                    200,
                    serde_json::json!({
                        "data": [{
                            "day": "2026-09-29",
                            "kind": "binary",
                            "source": "r2",
                            "version": "5.1.1",
                            "asset": "omo-linux-x64",
                            "count": "6"
                        }]
                    })
                    .to_string(),
                ),
            ),
        )
        .expect("rollup");
        assert_eq!(stored, 1);

        let response = driver.exchange("GET", "/stats/downloads", &[]);
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("stats json");
        assert_eq!(body["servedFromMirror"], serde_json::json!(6));
    }

    #[test]
    fn a_route_level_database_failure_maps_to_500() {
        let driver = driver(StaticFetcher::new());
        driver
            .harness()
            .db
            .execute("DROP TABLE downloads_daily")
            .expect("drop the table");

        let response = driver.exchange("GET", "/stats/downloads", &[]);
        assert_eq!(response.status, 500);
    }
}
