//! Tool output (spec §5 "Text content" and "Details JSON").

use crate::ToolFailure;
use crate::normalize::Normalized;
use serde_json::{Value, json};

/// Text, details and error flag of one call.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutput {
    pub text: String,
    pub details: Value,
    pub is_error: bool,
}

/// The successful result text: header, numbered blocks, then the queries line.
pub fn success_text(normalized: &Normalized) -> String {
    let mut lines = vec![format!("x_search results: {}", normalized.results.len())];
    if normalized.results.is_empty() {
        lines.push("No X posts matched this search.".to_owned());
    }
    for (index, result) in normalized.results.iter().enumerate() {
        lines.push(String::new());
        lines.push(format!("{}. {}", index + 1, result.title));
        lines.push(format!("URL: {}", result.url));
        lines.push(format!("Snippet: {}", result.snippet));
    }
    if !normalized.queries.is_empty() {
        lines.push(String::new());
        lines.push(format!("Queries used: {}", normalized.queries.join(" | ")));
    }
    lines.join("\n")
}

/// A successful call.
pub fn success(normalized: &Normalized) -> ToolOutput {
    ToolOutput {
        text: success_text(normalized),
        details: json!({
            "results": normalized.results.iter().map(crate::normalize::PostResult::to_json).collect::<Vec<_>>(),
            "queries": normalized.queries,
            "usage": {
                "xSearchCalls": normalized.usage.x_search_calls,
                "costTicks": normalized.usage.cost_ticks,
            },
        }),
        is_error: false,
    }
}

/// A failed call: `x_search error [<CODE>]: <message>`, empty lists, zero usage and `code`.
pub fn failure(failure: &ToolFailure) -> ToolOutput {
    ToolOutput {
        text: format!("x_search error [{}]: {}", failure.code, failure.message),
        details: json!({
            "results": [],
            "queries": [],
            "usage": {"xSearchCalls": 0, "costTicks": 0},
            "code": failure.code.as_str(),
        }),
        is_error: true,
    }
}
