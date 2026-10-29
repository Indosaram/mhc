//! X (Twitter) post search through xAI's server-side X search (Responses API).
//!
//! The `<builtin:x-search>` extension registers the deferred `x_search` tool and contributes the
//! `x-search` usage skill, but only when an xAI credential is connected at load time
//! ([`availability`]). Each call validates its input ([`params`]), resolves the bearer, posts one
//! request ([`request`], [`call`]), and turns the response into numbered results
//! ([`normalize`], [`output`]). Built clean-room from `docs/cleanroom/specs/x-search.md`.

pub mod availability;
pub mod call;
pub mod extension;
pub mod normalize;
pub mod output;
pub mod params;
pub mod request;
pub mod skill;

pub use call::{CallDeps, run};
pub use extension::XSearchExtension;
pub use output::ToolOutput;

/// Tool name (interface fact).
pub const TOOL_NAME: &str = "x_search";
/// Tool-search group and extension mount name.
pub const SEARCH_GROUP: &str = "x-search";

/// Environment lookup used everywhere instead of `std::env` so tests can inject values.
pub type EnvLookup = std::sync::Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// The process environment as an [`EnvLookup`].
pub fn process_env_lookup() -> EnvLookup {
    std::sync::Arc::new(|key: &str| std::env::var(key).ok())
}

/// The error codes a call can end with (interface facts: tests and prompts match them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidParams,
    InvalidFilters,
    TooManyHandles,
    InvalidDate,
    InvalidDateRange,
    Auth,
    RateLimited,
    Timeout,
    Upstream,
    Protocol,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidParams => "INVALID_PARAMS",
            Self::InvalidFilters => "INVALID_FILTERS",
            Self::TooManyHandles => "TOO_MANY_HANDLES",
            Self::InvalidDate => "INVALID_DATE",
            Self::InvalidDateRange => "INVALID_DATE_RANGE",
            Self::Auth => "AUTH",
            Self::RateLimited => "RATE_LIMITED",
            Self::Timeout => "TIMEOUT",
            Self::Upstream => "UPSTREAM",
            Self::Protocol => "PROTOCOL",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A failed call: its code and a human message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolFailure {
    pub code: ErrorCode,
    pub message: String,
}

impl ToolFailure {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
