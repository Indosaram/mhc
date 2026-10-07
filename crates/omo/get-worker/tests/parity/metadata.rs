//! Native regression: the stored object's HTTP metadata reaches the served response.
//!
//! Upstream `objectResponse` applies `object.writeHttpMetadata(headers)` first and only
//! then sets `Content-Type`, `Content-Disposition` and `Cache-Control`, so the stored
//! metadata survives for the other fields and loses for those three. This exercises that
//! ordering through the real `route` entry point across GET, HEAD and the edge-cache
//! boundary: a `write_http_metadata` that regressed to a no-op, or one applied after the
//! overrides, fails here.

use get_worker::{route, HttpMetadata, Method, Request, Response, IMMUTABLE};

use crate::fakes::{Harness, StaticFetcher};

const EXPIRES: &str = "Wed, 21 Oct 2026 07:28:00 GMT";

fn request(method: Method, path: &str) -> Request {
    Request::new(method, format!("https://get.omo.dev{path}"))
}

/// Every field present, so both the headers that survive and the three that are overridden
/// are asserted.
fn stored_metadata() -> HttpMetadata {
    HttpMetadata {
        content_type: Some("text/plain; charset=utf-8".to_string()),
        content_language: Some("en-US".to_string()),
        content_disposition: Some("inline".to_string()),
        content_encoding: Some("br".to_string()),
        cache_control: Some("public, max-age=60".to_string()),
        cache_expiry: Some(EXPIRES.to_string()),
    }
}

fn mirror(h: &Harness) {
    h.bucket.put_with_metadata(
        "releases/v5.1.1/omo-linux-x64",
        "binary-bytes",
        stored_metadata(),
    );
    h.bucket.put("releases/v5.1.1/.complete", "{}");
}

/// The `writeHttpMetadata` output `objectResponse` preserves.
fn assert_preserved(response: &Response) {
    assert_eq!(response.headers.get("Content-Language"), Some("en-US"));
    assert_eq!(response.headers.get("Content-Encoding"), Some("br"));
    assert_eq!(response.headers.get("Expires"), Some(EXPIRES));
}

/// The three headers `objectResponse` sets after the metadata, which must win.
fn assert_overridden(response: &Response) {
    assert_eq!(
        response.headers.get("Content-Type"),
        Some("application/octet-stream")
    );
    assert_eq!(
        response.headers.get("Content-Disposition"),
        Some("attachment; filename=\"omo-linux-x64\"")
    );
    assert_eq!(response.headers.get("Cache-Control"), Some(IMMUTABLE));
}

#[test]
fn a_get_serves_the_stored_http_metadata_and_the_service_overrides_win() {
    let h = Harness::new("", StaticFetcher::new());
    mirror(&h);

    let response =
        route(&request(Method::Get, "/v/5.1.1/omo-linux-x64"), &h.context()).expect("route");

    assert_eq!(response.status, 200);
    assert_eq!(response.body_text(), "binary-bytes");
    assert_preserved(&response);
    assert_overridden(&response);
}

#[test]
fn a_head_returns_the_same_metadata_headers_with_no_body() {
    let h = Harness::new("", StaticFetcher::new());
    mirror(&h);

    let response =
        route(&request(Method::Head, "/v/5.1.1/omo-linux-x64"), &h.context()).expect("route");

    assert_eq!(response.status, 200);
    assert!(response.body.is_none());
    assert_preserved(&response);
    assert_overridden(&response);
}

#[test]
fn the_metadata_survives_the_edge_cache_boundary() {
    let h = Harness::new("", StaticFetcher::new());
    mirror(&h);
    let ctx = h.context();

    let first = route(&request(Method::Get, "/v/5.1.1/omo-linux-x64"), &ctx).expect("route");
    assert_eq!(first.headers.get("X-Omo-Source"), Some("r2"));
    h.settle();
    let reads = h.bucket.reads().len();

    let cached = route(&request(Method::Get, "/v/5.1.1/omo-linux-x64"), &ctx).expect("route");
    assert_eq!(cached.headers.get("X-Omo-Source"), Some("cache"));
    assert_eq!(h.bucket.reads().len(), reads);
    assert_preserved(&cached);
    assert_overridden(&cached);
}

#[test]
fn an_object_without_stored_metadata_writes_no_metadata_headers() {
    let h = Harness::new("", StaticFetcher::new());
    h.bucket.put("releases/v5.1.1/omo-linux-x64", "binary-bytes");
    h.bucket.put("releases/v5.1.1/.complete", "{}");

    let response =
        route(&request(Method::Get, "/v/5.1.1/omo-linux-x64"), &h.context()).expect("route");

    assert_eq!(response.headers.get("Content-Language"), None);
    assert_eq!(response.headers.get("Content-Encoding"), None);
    assert_eq!(response.headers.get("Expires"), None);
    assert_eq!(
        response.headers.get("Content-Type"),
        Some("application/octet-stream")
    );
}
