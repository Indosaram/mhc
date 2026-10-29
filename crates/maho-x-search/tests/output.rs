//! §8.10 output text and details.

use maho_x_search::normalize::{Normalized, PostResult, Usage};
use maho_x_search::output::{failure, success};
use maho_x_search::{ErrorCode, ToolFailure};
use serde_json::json;

fn post(id: &str, title: &str, snippet: &str) -> PostResult {
    PostResult {
        id: id.into(),
        url: format!("https://x.com/a/status/{id}"),
        title: title.into(),
        snippet: snippet.into(),
    }
}

#[test]
fn empty_result_text() {
    let out = success(&Normalized::default());
    let lines: Vec<&str> = out.text.lines().collect();
    assert_eq!(lines[0], "x_search results: 0");
    assert_eq!(lines.len(), 2);
    assert!(!lines[1].is_empty());
    assert!(!out.is_error);
    assert_eq!(
        out.details,
        json!({"results": [], "queries": [], "usage": {"xSearchCalls": 0, "costTicks": 0}})
    );
}

#[test]
fn numbered_blocks_and_queries_line() {
    let normalized = Normalized {
        results: vec![post("1", "First", "line one"), post("2", "Second", "")],
        queries: vec!["query=a".into(), "query=b limit=3".into()],
        usage: Usage {
            x_search_calls: 2,
            cost_ticks: 900,
        },
    };
    let out = success(&normalized);
    let lines: Vec<&str> = out.text.lines().filter(|line| !line.is_empty()).collect();
    assert_eq!(
        lines,
        [
            "x_search results: 2",
            "1. First",
            "URL: https://x.com/a/status/1",
            "Snippet: line one",
            "2. Second",
            "URL: https://x.com/a/status/2",
            "Snippet: ",
            "Queries used: query=a | query=b limit=3",
        ]
    );
    assert!(out.details.get("code").is_none());
    assert_eq!(
        out.details["usage"],
        json!({"xSearchCalls": 2, "costTicks": 900})
    );
    assert_eq!(
        out.details["results"][0],
        json!({"id": "1", "url": "https://x.com/a/status/1", "title": "First", "snippet": "line one"})
    );
    assert_eq!(
        out.details["queries"],
        json!(["query=a", "query=b limit=3"])
    );
}

#[test]
fn error_text_and_details_carry_the_code() {
    let out = failure(&ToolFailure::new(ErrorCode::RateLimited, "slow down"));
    assert!(out.is_error);
    assert_eq!(out.text, "x_search error [RATE_LIMITED]: slow down");
    assert_eq!(
        out.details,
        json!({"results": [], "queries": [], "usage": {"xSearchCalls": 0, "costTicks": 0}, "code": "RATE_LIMITED"})
    );
}
