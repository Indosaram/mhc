//! Port of omo-senpi `components/x-search/client.test.ts` at latest `455dee62`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use maho_ai::utils::abort::AbortController;
use maho_omo_x_search::client::{
    build_x_search_request, carrier_model, perform_x_search, prompt_v1, prompt_v2,
    PerformXSearchOptions, XSearchCarrier, XSearchCarrierName,
    XSearchFetch, XSearchFetchOutcome, XSearchHttpResponse, XSearchPromptVariant,
    XSearchRequestParams, DEFAULT_CARRIER, DEFAULT_PROMPT_VARIANT,
};
use maho_omo_x_search::format::{
    format_x_search_error, format_x_search_result, NormalizedXSearch, XSearchErrorCode,
    XSearchResultItem, XSearchUsage,
};
use maho_omo_x_search::normalize_x_search_response;
use serde_json::{json, Value};

fn fast_carrier() -> XSearchCarrier {
    carrier_model(XSearchCarrierName::Fast)
}

fn reasoning_carrier() -> XSearchCarrier {
    carrier_model(XSearchCarrierName::Reasoning)
}

fn base_params() -> XSearchRequestParams {
    XSearchRequestParams {
        query: "Grok CLI".to_string(),
        mode: "latest".to_string(),
        max_results: 10,
        from_date: None,
        to_date: None,
        allowed_x_handles: None,
        excluded_x_handles: None,
        enable_image_understanding: None,
        enable_video_understanding: None,
    }
}

fn fast_fixture() -> Value {
    let content = include_str!("./__fixtures__/x-search-response.fast.json");
    serde_json::from_str(content).expect("parse fast fixture")
}

fn probe_fixture() -> Value {
    let content = include_str!("./__fixtures__/x-search-response.probe.json");
    serde_json::from_str(content).expect("parse probe fixture")
}

#[test]
fn test_build_request_given_dates_and_allowed_handles_when_building_then_bounded_envelope() {
    let mut params = base_params();
    params.from_date = Some("2026-09-01".to_string());
    params.to_date = Some("2026-09-03".to_string());
    params.allowed_x_handles = Some(vec!["grok".to_string(), "xai".to_string()]);

    let body = build_x_search_request(&params, &fast_carrier(), XSearchPromptVariant::V1);

    assert_eq!(body.model, "grok-4.20-0309-non-reasoning");
    assert_eq!(body.tool_choice, "required");
    assert_eq!(body.max_turns, 1);
    assert!(!body.parallel_tool_calls);
    assert_eq!(body.max_output_tokens, 4000);
    assert!(!body.store);

    assert_eq!(body.tools.len(), 1);
    let tool_obj = body.tools[0].as_object().expect("tool obj");
    assert_eq!(tool_obj.get("type").and_then(Value::as_str), Some("x_search"));
    assert_eq!(tool_obj.get("from_date").and_then(Value::as_str), Some("2026-09-01"));
    assert_eq!(tool_obj.get("to_date").and_then(Value::as_str), Some("2026-09-03"));
    assert_eq!(
        tool_obj.get("allowed_x_handles"),
        Some(&json!(["grok", "xai"]))
    );

    assert_eq!(body.input.len(), 1);
    assert_eq!(body.input[0].role, "user");
    assert!(body.input[0].content.contains("Run exactly this query: \"Grok CLI since:2026-09-01 until:2026-09-03\""));
    assert!(body.input[0].content.contains("mode=Latest"));
    assert!(body.input[0].content.contains("Return up to 10 X posts"));
    assert!(body.reasoning.is_none());
}

#[test]
fn test_build_request_given_top_mode_with_no_filters_when_building_then_top_prompt() {
    let mut params = base_params();
    params.query = "bun 1.4".to_string();
    params.mode = "top".to_string();
    params.max_results = 5;

    let body = build_x_search_request(&params, &fast_carrier(), XSearchPromptVariant::V1);

    assert_eq!(body.tools, vec![json!({ "type": "x_search" })]);
    assert_eq!(body.tool_choice, "required");
    assert_eq!(body.max_turns, 1);
    assert!(!body.parallel_tool_calls);
    assert_eq!(body.max_output_tokens, 4000);
    assert!(!body.store);

    assert!(body.input[0].content.contains("mode=Top"));
    assert!(body.input[0].content.contains("Run exactly this query: \"bun 1.4\""));
    assert!(body.input[0].content.contains("Return up to 5 X posts"));
}

#[test]
fn test_build_request_given_excluded_handles_plus_understanding_flags_on_reasoning_carrier() {
    let mut params = base_params();
    params.query = "xAI livestream".to_string();
    params.excluded_x_handles = Some(vec!["spam_account".to_string()]);
    params.enable_image_understanding = Some(true);
    params.enable_video_understanding = Some(false);

    let body = build_x_search_request(&params, &reasoning_carrier(), XSearchPromptVariant::V2);

    assert_eq!(body.model, "grok-4.6");
    assert_eq!(
        body.reasoning.as_ref().map(|r| r.effort.as_str()),
        Some("low")
    );
    assert_eq!(body.tool_choice, "required");
    assert_eq!(body.max_turns, 1);
    assert!(!body.parallel_tool_calls);
    assert_eq!(body.max_output_tokens, 4000);
    assert!(!body.store);

    assert_eq!(body.tools.len(), 1);
    let tool_obj = body.tools[0].as_object().expect("tool obj");
    assert_eq!(tool_obj.get("type").and_then(Value::as_str), Some("x_search"));
    assert_eq!(
        tool_obj.get("excluded_x_handles"),
        Some(&json!(["spam_account"]))
    );
    assert_eq!(
        tool_obj.get("enable_image_understanding"),
        Some(&json!(true))
    );
    assert_eq!(
        tool_obj.get("enable_video_understanding"),
        Some(&json!(false))
    );

    assert_eq!(body.input[0].content, prompt_v2(&params));
}

#[test]
fn test_defaults_and_prompt_variants_relationship() {
    assert_eq!(DEFAULT_PROMPT_VARIANT, XSearchPromptVariant::V1);
    assert_eq!(DEFAULT_CARRIER, XSearchCarrierName::Fast);
    assert_eq!(
        carrier_model(XSearchCarrierName::Fast).model,
        "grok-4.20-0309-non-reasoning"
    );
    assert_eq!(carrier_model(XSearchCarrierName::Reasoning).model, "grok-4.6");

    let params = base_params();
    let v1 = prompt_v1(&params);
    let v2 = prompt_v2(&params);
    assert!(v2.starts_with(&v1));
    assert!(v2.contains("Prefer the most recent posts"));
}

#[tokio::test]
async fn test_perform_x_search_given_200_json_response() {
    let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, BTreeMap<String, String>, String)>::new()));
    let captured_clone = captured.clone();

    let fetch_impl: XSearchFetch = Arc::new(move |url, headers, body| {
        captured_clone.lock().unwrap().push((url, headers, body));
        let fixture_body = serde_json::to_string(&fast_fixture()).unwrap();
        Box::pin(async move {
            Ok(XSearchHttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: fixture_body,
            })
        })
    });

    let outcome = perform_x_search(PerformXSearchOptions {
        fetch: fetch_impl,
        bearer: "test-bearer".to_string(),
        body: json!({ "model": "m" }),
        signal: None,
        deadline_ms: None,
        endpoint: None,
    })
    .await;

    let XSearchFetchOutcome::Ok(raw) = outcome else {
        panic!("expected ok outcome");
    };
    assert_eq!(
        raw.get("model").and_then(Value::as_str),
        Some("grok-4.20-0309-non-reasoning")
    );

    let recorded = captured.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].1.get("authorization").map(String::as_str),
        Some("Bearer test-bearer")
    );
    assert_eq!(
        recorded[0].1.get("content-type").map(String::as_str),
        Some("application/json")
    );
}

async fn assert_http_error_fail_closed(status: u16, expected_code: XSearchErrorCode) {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_clone = calls.clone();

    let fetch_impl: XSearchFetch = Arc::new(move |_url, _headers, _body| {
        calls_clone.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(XSearchHttpResponse {
                status,
                headers: BTreeMap::new(),
                body: "nope".to_string(),
            })
        })
    });

    let outcome = perform_x_search(PerformXSearchOptions {
        fetch: fetch_impl,
        bearer: "b".to_string(),
        body: json!({}),
        signal: None,
        deadline_ms: None,
        endpoint: None,
    })
    .await;

    let XSearchFetchOutcome::Err { code, .. } = outcome else {
        panic!("expected error");
    };
    assert_eq!(code, expected_code);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_perform_x_search_http_401_maps_to_auth_error() {
    assert_http_error_fail_closed(401, XSearchErrorCode::Auth).await;
}

#[tokio::test]
async fn test_perform_x_search_http_403_maps_to_auth_error() {
    assert_http_error_fail_closed(403, XSearchErrorCode::Auth).await;
}

#[tokio::test]
async fn test_perform_x_search_http_500_maps_to_upstream_error() {
    assert_http_error_fail_closed(500, XSearchErrorCode::Upstream).await;
}

#[tokio::test]
async fn test_perform_x_search_http_503_maps_to_upstream_error() {
    assert_http_error_fail_closed(503, XSearchErrorCode::Upstream).await;
}

#[tokio::test]
async fn test_perform_x_search_429_carries_retry_after() {
    let fetch_impl: XSearchFetch = Arc::new(|_url, _headers, _body| {
        let mut headers = BTreeMap::new();
        headers.insert("retry-after".to_string(), "42".to_string());
        Box::pin(async move {
            Ok(XSearchHttpResponse {
                status: 429,
                headers,
                body: "slow down".to_string(),
            })
        })
    });

    let outcome = perform_x_search(PerformXSearchOptions {
        fetch: fetch_impl,
        bearer: "b".to_string(),
        body: json!({}),
        signal: None,
        deadline_ms: None,
        endpoint: None,
    })
    .await;

    let XSearchFetchOutcome::Err { code, retry_after, .. } = outcome else {
        panic!("expected error");
    };
    assert_eq!(code, XSearchErrorCode::RateLimited);
    assert_eq!(retry_after, Some(42));
}

#[tokio::test]
async fn test_perform_x_search_caller_abort_does_not_report_timeout() {
    let controller = AbortController::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_clone = calls.clone();

    // Already-aborted signal should return immediately and never execute fetch
    let fetch_impl: XSearchFetch = Arc::new(move |_url, _headers, _body| {
        calls_clone.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            std::future::pending::<()>().await;
            Ok(XSearchHttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: "{}".to_string(),
            })
        })
    });

    controller.abort(None);

    let outcome = perform_x_search(PerformXSearchOptions {
        fetch: fetch_impl,
        bearer: "b".to_string(),
        body: json!({}),
        signal: Some(controller.signal()),
        deadline_ms: Some(10_000),
        endpoint: None,
    })
    .await;

    let XSearchFetchOutcome::Err { code, message, .. } = outcome else {
        panic!("expected error");
    };
    assert_ne!(code, XSearchErrorCode::Timeout);
    assert_eq!(code, XSearchErrorCode::Upstream);
    assert_eq!(message, "xAI request was cancelled by the caller");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn test_perform_x_search_caller_abort_races_deadline_caller_wins() {
    let controller = AbortController::new();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started_tx = Arc::new(std::sync::Mutex::new(Some(started_tx)));

    let fetch_impl: XSearchFetch = Arc::new(move |_url, _headers, _body| {
        let started_tx = started_tx.clone();
        Box::pin(async move {
            if let Some(tx) = started_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
            // Pending future until caller cancellation aborts the operation
            std::future::pending::<()>().await;
            Ok(XSearchHttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: "{}".to_string(),
            })
        })
    });

    let controller_clone = controller.clone();
    let driver = tokio::spawn(async move {
        let _ = started_rx.await;
        controller_clone.abort(None);
    });

    let outcome = perform_x_search(PerformXSearchOptions {
        fetch: fetch_impl,
        bearer: "b".to_string(),
        body: json!({}),
        signal: Some(controller.signal()),
        deadline_ms: Some(60_000),
        endpoint: None,
    })
    .await;

    driver.await.expect("driver join");

    let XSearchFetchOutcome::Err { code, message, .. } = outcome else {
        panic!("expected error");
    };
    assert_eq!(code, XSearchErrorCode::Upstream);
    assert_eq!(message, "xAI request was cancelled by the caller");
}

#[tokio::test]
async fn test_perform_x_search_deadline_expiry_reports_timeout() {
    let fetch_impl: XSearchFetch = Arc::new(|_url, _headers, _body| {
        Box::pin(async move {
            // Pending future until deadline timer expires
            std::future::pending::<()>().await;
            Ok(XSearchHttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: "{}".to_string(),
            })
        })
    });

    let outcome = perform_x_search(PerformXSearchOptions {
        fetch: fetch_impl,
        bearer: "b".to_string(),
        body: json!({}),
        signal: None,
        deadline_ms: Some(1),
        endpoint: None,
    })
    .await;

    let XSearchFetchOutcome::Err { code, .. } = outcome else {
        panic!("expected error");
    };
    assert_eq!(code, XSearchErrorCode::Timeout);
}

#[tokio::test]
async fn test_perform_x_search_non_json_200_reports_protocol() {
    let fetch_impl: XSearchFetch = Arc::new(|_url, _headers, _body| {
        Box::pin(async move {
            Ok(XSearchHttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: "<html>gateway</html>".to_string(),
            })
        })
    });

    let outcome = perform_x_search(PerformXSearchOptions {
        fetch: fetch_impl,
        bearer: "b".to_string(),
        body: json!({}),
        signal: None,
        deadline_ms: None,
        endpoint: None,
    })
    .await;

    let XSearchFetchOutcome::Err { code, .. } = outcome else {
        panic!("expected error");
    };
    assert_eq!(code, XSearchErrorCode::Protocol);
}

#[test]
fn test_normalize_probe_fixture() {
    let normalized = normalize_x_search_response(&probe_fixture(), None);

    let ids: Vec<&str> = normalized.results.iter().map(|e| e.id.as_str()).collect();
    let unique_count = ids.iter().collect::<std::collections::HashSet<_>>().len();
    assert_eq!(unique_count, ids.len());
    assert!(ids.len() >= 4);
    assert!(ids.contains(&"2095333026204971141"));

    assert!(normalized.queries[0].contains("since:2026-09-01"));
    assert!(normalized.queries[0].contains("Latest"));
    assert_eq!(normalized.queries.len(), 3);
    assert_eq!(
        normalized.usage,
        XSearchUsage {
            x_search_calls: 3,
            cost_ticks: 262234000
        }
    );

    let first = normalized
        .results
        .iter()
        .find(|e| e.id == "2095333026204971141")
        .expect("first item");
    assert_eq!(first.url, "https://x.com/grok/status/2095333026204971141");
    assert!(first.snippet.contains("cheque-spitting CLI"));
}

#[test]
fn test_normalize_probe_fixture_max_results() {
    let normalized = normalize_x_search_response(&probe_fixture(), Some(2));
    assert_eq!(normalized.results.len(), 2);
    assert_eq!(normalized.queries.len(), 3);
}

#[test]
fn test_normalize_fast_fixture_dedupes_annotations_against_message_text() {
    let normalized = normalize_x_search_response(&fast_fixture(), None);

    let mut ids: Vec<String> = normalized.results.iter().map(|e| e.id.clone()).collect();
    ids.sort();
    assert_eq!(
        ids,
        vec![
            "2095271947169055077".to_string(),
            "2095307098238128218".to_string(),
            "2095344199851798606".to_string(),
        ]
    );

    let urls: Vec<&str> = normalized.results.iter().map(|e| e.url.as_str()).collect();
    assert_eq!(
        urls,
        vec![
            "https://x.com/i/status/2095344199851798606",
            "https://x.com/i/status/2095307098238128218",
            "https://x.com/i/status/2095271947169055077",
        ]
    );
    assert_eq!(
        normalized.usage,
        XSearchUsage {
            x_search_calls: 1,
            cost_ticks: 87231000
        }
    );
    assert!(normalized.queries[0].contains("Bun 1.4 since:2026-09-01"));
}

#[test]
fn test_normalize_citation_in_annotations_only() {
    let payload = json!({
        "output": [
            {
                "type": "message",
                "content": [
                    {
                        "type": "output_text",
                        "text": "https://x.com/grok/status/2095333026204971141 inline post",
                        "annotations": [
                            { "type": "url_citation", "url": "https://x.com/grok/status/2095333026204971141", "title": "1" },
                            { "type": "url_citation", "url": "https://x.com/i/status/2095332141047431453", "title": "2" }
                        ]
                    }
                ]
            }
        ]
    });

    let normalized = normalize_x_search_response(&payload, None);
    let ids: Vec<&str> = normalized.results.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["2095333026204971141", "2095332141047431453"]);
}

#[test]
fn test_normalize_payload_with_top_level_citations_only() {
    let payload = json!({
        "output": [],
        "citations": ["https://x.com/someone/status/1234567890123456789"]
    });

    let normalized = normalize_x_search_response(&payload, None);
    assert_eq!(
        normalized.results,
        vec![XSearchResultItem {
            id: "1234567890123456789".to_string(),
            url: "https://x.com/someone/status/1234567890123456789".to_string(),
            title: "https://x.com/someone/status/1234567890123456789".to_string(),
            snippet: String::new(),
        }]
    );
    assert!(normalized.queries.is_empty());
    assert_eq!(
        normalized.usage,
        XSearchUsage {
            x_search_calls: 0,
            cost_ticks: 0
        }
    );
}

#[test]
fn test_normalize_citation_titles_index_and_url_handling() {
    let payload = json!({
        "output": [
            {
                "type": "message",
                "content": [
                    {
                        "type": "output_text",
                        "text": "https://x.com/a/status/11 first\nhttps://x.com/b/status/22 second\nhttps://x.com/c/status/33 third",
                        "annotations": [
                            { "type": "url_citation", "url": "https://x.com/a/status/11", "title": "1" },
                            { "type": "url_citation", "url": "https://x.com/b/status/22", "title": "https://x.com/b/status/22" },
                            { "type": "url_citation", "url": "https://x.com/c/status/33", "title": "Real headline" }
                        ]
                    }
                ]
            }
        ]
    });

    let normalized = normalize_x_search_response(&payload, None);
    let titles: Vec<&str> = normalized.results.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(
        titles,
        vec![
            "https://x.com/a/status/11",
            "https://x.com/b/status/22",
            "Real headline",
        ]
    );
    assert_eq!(normalized.results[0].snippet, "https://x.com/a/status/11 first");
}

#[test]
fn test_normalize_response_with_no_x_posts() {
    let payload = json!({
        "output": [
            {
                "type": "message",
                "content": [
                    { "type": "output_text", "text": "No matching posts.", "annotations": [] }
                ]
            }
        ]
    });

    let normalized = normalize_x_search_response(&payload, None);
    assert!(normalized.results.is_empty());
    assert!(normalized.queries.is_empty());
}

#[test]
fn test_format_x_search_result_contract() {
    let text = format_x_search_result(&NormalizedXSearch {
        results: vec![
            XSearchResultItem {
                id: "1".to_string(),
                url: "https://x.com/grok/status/1".to_string(),
                title: "Grok ships the CLI".to_string(),
                snippet: "Grok jokes about a cheque-spitting CLI.".to_string(),
            },
            XSearchResultItem {
                id: "2".to_string(),
                url: "https://x.com/xai/status/2".to_string(),
                title: "https://x.com/xai/status/2".to_string(),
                snippet: "Second post summary.".to_string(),
            },
        ],
        queries: vec!["{\"query\":\"Grok CLI since:2026-09-01\",\"mode\":\"Latest\"}".to_string()],
        usage: XSearchUsage {
            x_search_calls: 1,
            cost_ticks: 100,
        },
    });

    let expected = [
        "x_search results: 2",
        "",
        "[1] Grok ships the CLI",
        "URL: https://x.com/grok/status/1",
        "Snippet: Grok jokes about a cheque-spitting CLI.",
        "",
        "[2] https://x.com/xai/status/2",
        "URL: https://x.com/xai/status/2",
        "Snippet: Second post summary.",
        "",
        "Queries used: {\"query\":\"Grok CLI since:2026-09-01\",\"mode\":\"Latest\"}",
    ]
    .join("\n");

    assert_eq!(text, expected);
}

#[test]
fn test_format_x_search_result_empty() {
    let text = format_x_search_result(&NormalizedXSearch {
        results: Vec::new(),
        queries: Vec::new(),
        usage: XSearchUsage::default(),
    });
    assert_eq!(text, "x_search results: 0\n(no matching X posts)");
}

#[test]
fn test_format_x_search_error_code_invalid_params() {
    assert_eq!(
        format_x_search_error(XSearchErrorCode::InvalidParams, "msg"),
        "x_search error [INVALID_PARAMS]: msg"
    );
}

#[test]
fn test_format_x_search_error_code_invalid_filters() {
    assert_eq!(
        format_x_search_error(XSearchErrorCode::InvalidFilters, "msg"),
        "x_search error [INVALID_FILTERS]: msg"
    );
}

#[test]
fn test_format_x_search_error_code_too_many_handles() {
    assert_eq!(
        format_x_search_error(XSearchErrorCode::TooManyHandles, "msg"),
        "x_search error [TOO_MANY_HANDLES]: msg"
    );
}

#[test]
fn test_format_x_search_error_code_invalid_date() {
    assert_eq!(
        format_x_search_error(XSearchErrorCode::InvalidDate, "msg"),
        "x_search error [INVALID_DATE]: msg"
    );
}

#[test]
fn test_format_x_search_error_code_invalid_date_range() {
    assert_eq!(
        format_x_search_error(XSearchErrorCode::InvalidDateRange, "msg"),
        "x_search error [INVALID_DATE_RANGE]: msg"
    );
}

#[test]
fn test_format_x_search_error_code_auth() {
    assert_eq!(
        format_x_search_error(XSearchErrorCode::Auth, "msg"),
        "x_search error [AUTH]: msg"
    );
}

#[test]
fn test_format_x_search_error_code_rate_limited() {
    assert_eq!(
        format_x_search_error(XSearchErrorCode::RateLimited, "msg"),
        "x_search error [RATE_LIMITED]: msg"
    );
}

#[test]
fn test_format_x_search_error_code_upstream() {
    assert_eq!(
        format_x_search_error(XSearchErrorCode::Upstream, "msg"),
        "x_search error [UPSTREAM]: msg"
    );
}

#[test]
fn test_format_x_search_error_code_timeout() {
    assert_eq!(
        format_x_search_error(XSearchErrorCode::Timeout, "msg"),
        "x_search error [TIMEOUT]: msg"
    );
}

#[test]
fn test_format_x_search_error_code_protocol() {
    assert_eq!(
        format_x_search_error(XSearchErrorCode::Protocol, "msg"),
        "x_search error [PROTOCOL]: msg"
    );
}

#[tokio::test]
async fn test_perform_x_search_custom_endpoint_override() {
    let captured_url = Arc::new(std::sync::Mutex::new(String::new()));
    let captured_url_clone = captured_url.clone();

    let fetch_impl: XSearchFetch = Arc::new(move |url, _headers, _body| {
        *captured_url_clone.lock().unwrap() = url;
        Box::pin(async move {
            Ok(XSearchHttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: "{}".to_string(),
            })
        })
    });

    let outcome = perform_x_search(PerformXSearchOptions {
        fetch: fetch_impl,
        bearer: "b".to_string(),
        body: json!({}),
        signal: None,
        deadline_ms: None,
        endpoint: Some("https://custom.xai.example.com/v1/chat/completions".to_string()),
    })
    .await;

    assert!(matches!(outcome, XSearchFetchOutcome::Ok(_)));
    assert_eq!(
        *captured_url.lock().unwrap(),
        "https://custom.xai.example.com/v1/chat/completions"
    );
}
