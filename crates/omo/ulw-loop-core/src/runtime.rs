//! Runtime error and ISO timestamp helpers.

use std::collections::BTreeMap;
use serde_json::Value;

#[derive(Debug, thiserror::Error, Clone)]
#[error("{message}")]
pub struct UlwLoopError {
    pub message: String,
    pub code: String,
    pub details: Option<BTreeMap<String, Value>>,
}

impl UlwLoopError {
    pub fn new(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: code.into(),
            details: None,
        }
    }

    pub fn with_details(
        message: impl Into<String>,
        code: impl Into<String>,
        details: BTreeMap<String, Value>,
    ) -> Self {
        Self {
            message: message.into(),
            code: code.into(),
            details: Some(details),
        }
    }
}

pub fn iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
