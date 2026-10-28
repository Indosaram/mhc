//! Failure results (spec §6).

use serde_json::{Map, Value, json};

pub const INVALID_ARGUMENT: &str = "INVALID_ARGUMENT";
pub const BINARY_NOT_FOUND: &str = "BINARY_NOT_FOUND";
pub const BINARY_INVALID: &str = "BINARY_INVALID";
pub const UNSUPPORTED_LANGUAGE: &str = "UNSUPPORTED_LANGUAGE";
pub const PATTERN_HINT_REJECTED: &str = "PATTERN_HINT_REJECTED";
pub const PATTERN_PARSE_FAILED: &str = "PATTERN_PARSE_FAILED";
pub const RULE_PARSE_FAILED: &str = "RULE_PARSE_FAILED";
pub const REWRITE_UNBOUND_METAVARIABLE: &str = "REWRITE_UNBOUND_METAVARIABLE";
pub const REWRITE_METAVARIABLE_KIND_MISMATCH: &str = "REWRITE_METAVARIABLE_KIND_MISMATCH";
pub const PATH_NOT_FOUND: &str = "PATH_NOT_FOUND";
pub const PATH_UNREADABLE: &str = "PATH_UNREADABLE";
pub const TIMEOUT: &str = "TIMEOUT";
pub const ABORTED: &str = "ABORTED";
pub const OUTPUT_TOO_LARGE: &str = "OUTPUT_TOO_LARGE";
pub const OUTPUT_PARSE_FAILED: &str = "OUTPUT_PARSE_FAILED";
pub const PREVIEW_TRUNCATED: &str = "PREVIEW_TRUNCATED";
pub const REWRITE_STALE_PREVIEW: &str = "REWRITE_STALE_PREVIEW";
pub const SG_FAILED: &str = "SG_FAILED";
pub const ENCODING_ERROR: &str = "ENCODING_ERROR";

/// Every error code, in the order of the spec.
pub const ERROR_CODES: [&str; 19] = [
    INVALID_ARGUMENT,
    BINARY_NOT_FOUND,
    BINARY_INVALID,
    UNSUPPORTED_LANGUAGE,
    PATTERN_HINT_REJECTED,
    PATTERN_PARSE_FAILED,
    RULE_PARSE_FAILED,
    REWRITE_UNBOUND_METAVARIABLE,
    REWRITE_METAVARIABLE_KIND_MISMATCH,
    PATH_NOT_FOUND,
    PATH_UNREADABLE,
    TIMEOUT,
    ABORTED,
    OUTPUT_TOO_LARGE,
    OUTPUT_PARSE_FAILED,
    PREVIEW_TRUNCATED,
    REWRITE_STALE_PREVIEW,
    SG_FAILED,
    ENCODING_ERROR,
];

/// Where in the call a failure happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Preflight,
    Search,
    Preview,
    Apply,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::Search => "search",
            Self::Preview => "preview",
            Self::Apply => "apply",
        }
    }
}

/// A failed tool call.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolError {
    pub code: &'static str,
    pub message: String,
    pub phase: Phase,
    pub language: Option<String>,
    pub details: Box<Value>,
}

impl ToolError {
    pub fn new(code: &'static str, phase: Phase, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            phase,
            language: None,
            details: Box::new(json!({})),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(INVALID_ARGUMENT, Phase::Preflight, message)
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Box::new(details);
        self
    }

    pub fn with_language(mut self, language: &str) -> Self {
        self.language = Some(language.to_owned());
        self
    }

    /// True only for `TIMEOUT`, `ABORTED`, `OUTPUT_PARSE_FAILED` and `REWRITE_STALE_PREVIEW`.
    pub fn retryable(&self) -> bool {
        matches!(
            self.code,
            TIMEOUT | ABORTED | OUTPUT_PARSE_FAILED | REWRITE_STALE_PREVIEW
        )
    }

    /// The failure result object.
    pub fn to_json(&self, kind: Option<&str>, duration_ms: Option<u64>) -> Value {
        let mut error = Map::new();
        error.insert("code".into(), json!(self.code));
        error.insert("message".into(), json!(self.message));
        error.insert("retryable".into(), json!(self.retryable()));
        error.insert("phase".into(), json!(self.phase.as_str()));
        if let Some(language) = &self.language {
            error.insert("language".into(), json!(language));
        }
        error.insert("details".into(), Value::clone(&self.details));
        let mut out = Map::new();
        out.insert("schemaVersion".into(), json!(1));
        out.insert("ok".into(), json!(false));
        if let Some(kind) = kind {
            out.insert("kind".into(), json!(kind));
        }
        out.insert("error".into(), Value::Object(error));
        if let Some(duration_ms) = duration_ms {
            out.insert("durationMs".into(), json!(duration_ms));
        }
        Value::Object(out)
    }
}
