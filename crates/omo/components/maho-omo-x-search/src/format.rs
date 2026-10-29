//! Port of omo-senpi `components/x-search/format.ts` at latest `455dee62`.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum XSearchErrorCode {
    InvalidParams,
    InvalidFilters,
    TooManyHandles,
    InvalidDate,
    InvalidDateRange,
    Auth,
    RateLimited,
    Upstream,
    Timeout,
    Protocol,
}

impl XSearchErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InvalidParams => "INVALID_PARAMS",
            Self::InvalidFilters => "INVALID_FILTERS",
            Self::TooManyHandles => "TOO_MANY_HANDLES",
            Self::InvalidDate => "INVALID_DATE",
            Self::InvalidDateRange => "INVALID_DATE_RANGE",
            Self::Auth => "AUTH",
            Self::RateLimited => "RATE_LIMITED",
            Self::Upstream => "UPSTREAM",
            Self::Timeout => "TIMEOUT",
            Self::Protocol => "PROTOCOL",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct XSearchResultItem {
    pub id: String,
    pub url: String,
    pub title: String,
    pub snippet: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct XSearchUsage {
    #[serde(rename = "xSearchCalls")]
    pub x_search_calls: usize,
    #[serde(rename = "costTicks")]
    pub cost_ticks: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedXSearch {
    pub results: Vec<XSearchResultItem>,
    pub queries: Vec<String>,
    pub usage: XSearchUsage,
}

pub const EMPTY_RESULT_TEXT: &str = "x_search results: 0\n(no matching X posts)";

pub fn format_x_search_result(normalized: &NormalizedXSearch) -> String {
    if normalized.results.is_empty() {
        return EMPTY_RESULT_TEXT.to_string();
    }

    let mut lines = Vec::new();
    lines.push(format!("x_search results: {}", normalized.results.len()));
    lines.push(String::new());

    for (index, entry) in normalized.results.iter().enumerate() {
        lines.push(format!("[{}] {}", index + 1, entry.title));
        lines.push(format!("URL: {}", entry.url));
        lines.push(format!("Snippet: {}", entry.snippet));
        lines.push(String::new());
    }

    lines.push(format!("Queries used: {}", normalized.queries.join(" | ")));
    lines.join("\n")
}

pub fn format_x_search_error(code: XSearchErrorCode, message: &str) -> String {
    format!("x_search error [{}]: {}", code.as_str(), message)
}
