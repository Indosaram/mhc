//! Support-contract tests for the maho-ai wire-API replay harness (todos 10-12).
//!
//! No maho-ai wire API is ported yet, so this target covers the harness itself: [`support::mock_server`]
//! serves recorded fixture bytes (and can abort the connection mid-stream), and
//! [`support::replay`] compares an event sequence with the golden `tools/golden/ai-replay.mjs`
//! recorded from senpi for the same fixture. The round-trip test below drives the real goldens
//! through the Rust event types, which is the parity check available before the wire APIs land.

mod support;

use axum::http::StatusCode;
use maho_ai::types::AssistantMessageEvent;
use support::mock_server::{Fixture, MockFixtureServer, with_timeout};
use support::replay::{assert_matches_golden, golden_path};

const BASIC_SSE: &str = "data: {\"id\":\"chatcmpl-basic\"}\n\ndata: [DONE]\n\n";

const REPLAY_CASES: [&str; 5] = [
    "ai-replay-anthropic-basic",
    "ai-replay-openai-completions-basic",
    "ai-replay-openai-completions-eof-truncated",
    "ai-replay-openai-completions-tool-call",
    "ai-replay-openai-responses-basic",
];

fn golden_events(case: &str) -> Vec<AssistantMessageEvent> {
    let path = golden_path(case);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "golden {} is unreadable: {error} (generate it with bun tools/golden/ai-replay.mjs)",
            path.display()
        )
    });
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("golden {} does not match the Rust event types: {error}", path.display()))
}

#[tokio::test]
async fn serves_fixture_bytes_verbatim() {
    let server = MockFixtureServer::start(Fixture::sse([
        (None, "{\"id\":\"chatcmpl-basic\"}"),
        (None, "[DONE]"),
    ]))
    .await;
    let response = with_timeout(10, reqwest::get(format!("{}/chat/completions", server.base_url())))
        .await
        .expect("mock fixture server responds");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"].to_str().expect("content-type is ASCII"),
        "text/event-stream"
    );
    let body = with_timeout(10, response.text()).await.expect("read fixture body");
    assert_eq!(body, BASIC_SSE);
}

#[tokio::test]
async fn ends_the_stream_mid_way() {
    let cut = BASIC_SSE.find("[DONE]").expect("fixture carries a terminator frame");
    let server = MockFixtureServer::start(
        Fixture::sse([(None, "{\"id\":\"chatcmpl-basic\"}"), (None, "[DONE]")]).closed_after(cut),
    )
    .await;
    let response = with_timeout(10, reqwest::get(format!("{}/chat/completions", server.base_url())))
        .await
        .expect("mock fixture server responds");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"].to_str().expect("content-type is ASCII"),
        "text/event-stream"
    );
    let body = with_timeout(10, response.text()).await.expect("read truncated body");
    assert_eq!(body, &BASIC_SSE[..cut]);
    assert!(
        !body.contains("[DONE]"),
        "a stream cut before its terminator frame must not deliver it (SSE transport EOF)"
    );
}

#[tokio::test]
async fn serves_json_error_body_with_its_status() {
    const ERROR_BODY: &str = "{\"error\":{\"message\":\"rate limited\",\"type\":\"rate_limit_error\"}}";
    let server =
        MockFixtureServer::start(Fixture::json(StatusCode::TOO_MANY_REQUESTS, ERROR_BODY)).await;
    let response = with_timeout(10, reqwest::get(format!("{}/chat/completions", server.base_url())))
        .await
        .expect("mock fixture server responds");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response.headers()["content-type"].to_str().expect("content-type is ASCII"),
        "application/json"
    );
    let body = with_timeout(10, response.text()).await.expect("read error body");
    assert_eq!(body, ERROR_BODY);
}

#[test]
fn golden_event_json_round_trips_through_the_rust_event_types() {
    for case in REPLAY_CASES {
        let events = golden_events(case);
        assert!(!events.is_empty(), "{case} recorded no events");
        assert_matches_golden(case, &events);
    }
}

#[test]
#[should_panic(expected = "replay mismatch")]
fn rejects_an_event_sequence_that_differs_from_the_golden() {
    let mut events = golden_events("ai-replay-openai-completions-basic");
    events.pop();
    assert_matches_golden("ai-replay-openai-completions-basic", &events);
}
