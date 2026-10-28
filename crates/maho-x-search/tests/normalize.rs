//! §8.9 normalization.

use maho_x_search::normalize::{Usage, extract_queries, extract_usage, normalize};
use serde_json::{Value, json};

fn message(text: &str, annotations: Value) -> Value {
    json!({
        "type": "message",
        "content": [{"type": "output_text", "text": text, "annotations": annotations}],
    })
}

#[test]
fn dedupe_keeps_first_seen_order_across_sources() {
    let text = "Top posts:\n- https://x.com/alice/status/111 alice on rust\n- https://x.com/bob/status/222 bob on rust\n";
    let response = json!({
        "output": [message(text, json!([
            {"type": "url_citation", "url": "https://x.com/bob/status/222", "title": "Bob says"},
            {"type": "file_citation", "url": "https://x.com/zed/status/999", "title": "ignored type"},
        ]))],
        "citations": ["https://x.com/carol/status/333", "https://www.x.com/alice/status/111", "https://example.com/a"],
    });
    let normalized = normalize(&response, 10);
    let ids: Vec<&str> = normalized.results.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["222", "111", "333"]);
    assert_eq!(normalized.results[0].title, "Bob says");
    assert_eq!(normalized.results[0].url, "https://x.com/bob/status/222");
}

#[test]
fn url_shapes_accepted_and_rejected() {
    let text = "a http://www.x.com/i/status/1 b https://x.com/user_1/status/2 \
                c https://twitter.com/u/status/3 d https://x.com/u/status/abc \
                e https://x.com/u/status/1234567890123456789012345678901";
    let ids: Vec<String> = normalize(&json!({"output": [message(text, json!([]))]}), 30)
        .results
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(ids, ["1", "2"]);
}

#[test]
fn title_falls_back_to_the_url() {
    let response = json!({"output": [message("", json!([
        {"type": "url_citation", "url": "https://x.com/a/status/1", "title": ""},
        {"type": "url_citation", "url": "https://x.com/a/status/2", "title": "12345"},
        {"type": "url_citation", "url": "https://x.com/a/status/3", "title": "https://x.com/a/status/3"},
        {"type": "url_citation", "url": "https://x.com/a/status/4"},
        {"type": "url_citation", "url": "https://x.com/a/status/5", "title": "  Real title "},
    ]))]});
    let titles: Vec<String> = normalize(&response, 10)
        .results
        .into_iter()
        .map(|r| r.title)
        .collect();
    assert_eq!(
        titles,
        [
            "https://x.com/a/status/1",
            "https://x.com/a/status/2",
            "https://x.com/a/status/3",
            "https://x.com/a/status/4",
            "Real title",
        ]
    );
}

#[test]
fn snippet_is_the_first_matching_trimmed_line() {
    let text = "Intro line /status/100\n   First: https://x.com/a/status/10 is about cats   \nAgain https://x.com/a/status/10\nOther post /status/20 mentioned\n";
    let response = json!({
        "output": [message(text, json!([]))],
        "citations": ["https://x.com/b/status/20", "https://x.com/c/status/30"],
    });
    let results = normalize(&response, 10).results;
    assert_eq!(
        results[0].snippet,
        "First: https://x.com/a/status/10 is about cats"
    );
    assert_eq!(results[1].id, "20");
    assert_eq!(results[1].snippet, "Other post /status/20 mentioned");
    assert_eq!(results[2].snippet, "");
}

#[test]
fn results_are_truncated_to_max_results() {
    let citations: Vec<String> = (1..=8)
        .map(|id| format!("https://x.com/a/status/{id}"))
        .collect();
    let results = normalize(&json!({"citations": citations}), 3).results;
    assert_eq!(
        results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        ["1", "2", "3"]
    );
}

#[test]
fn queries_come_only_from_listed_names_and_types() {
    let response = json!({"output": [
        {"type": "custom_tool_call", "name": "x_keyword_search", "input": "{\"query\":\"rust lang\",\"limit\":5,\"mode\":\"Latest\"}"},
        {"type": "x_search_call", "name": "x_user_search", "input": "{\"query\":\"alice\"}"},
        {"type": "custom_tool_call", "name": "web_search", "input": "{\"query\":\"nope\"}"},
        {"type": "function_call", "name": "x_semantic_search", "input": "{\"query\":\"nope\"}"},
        {"type": "custom_tool_call", "name": "x_thread_fetch", "input": "not json"},
        {"type": "custom_tool_call", "name": "x_thread_fetch", "input": "[1,2]"},
        {"type": "custom_tool_call", "name": "x_semantic_search", "input": {"query": "object, not string"}},
        {"type": "custom_tool_call", "name": "x_thread_fetch", "input": "{\"post_id\":\"42\",\"deep\":true}"},
    ]});
    assert_eq!(
        extract_queries(&response),
        [
            "query=rust lang limit=5 mode=Latest",
            "query=alice",
            "post_id=42 deep=true"
        ]
    );
}

#[test]
fn usage_fields_are_read_or_zero() {
    let usage = extract_usage(&json!({"usage": {
        "server_side_tool_usage_details": {"x_search_calls": 3},
        "cost_in_usd_ticks": 125000,
    }}));
    assert_eq!(
        usage,
        Usage {
            x_search_calls: 3,
            cost_ticks: 125000
        }
    );
    assert_eq!(extract_usage(&json!({})), Usage::default());
    assert_eq!(extract_usage(&json!({"usage": "lots"})), Usage::default());
}

#[test]
fn garbage_payloads_give_empty_results() {
    for payload in [
        json!(null),
        json!(42),
        json!("text"),
        json!([1, 2]),
        json!({"output": "nope", "citations": {"a": 1}, "usage": []}),
        json!({"output": [null, 1, {"type": "message", "content": "x"}, {"type": "message", "content": [null, {"text": 5, "annotations": "x"}]}]}),
        json!({"output": [{"type": "message", "content": [{"annotations": [null, {"type": "url_citation", "url": 7}]}]}], "citations": [null, 3]}),
    ] {
        let normalized = normalize(&payload, 10);
        assert!(normalized.results.is_empty(), "{payload}");
        assert!(normalized.queries.is_empty(), "{payload}");
        assert_eq!(normalized.usage, Usage::default(), "{payload}");
    }
}
