//! Runtime error and ISO timestamp helpers.

use std::collections::BTreeMap;
use std::error::Error;
use std::sync::Arc;
use serde_json::Value;

#[derive(Debug, thiserror::Error, Clone)]
#[error("{message}")]
pub struct UlwLoopError {
    pub message: String,
    pub code: String,
    #[source]
    pub cause: Option<Arc<dyn Error + Send + Sync>>,
    pub details: Option<BTreeMap<String, Value>>,
}

impl UlwLoopError {
    pub fn new(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: code.into(),
            cause: None,
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
            cause: None,
            details: Some(details),
        }
    }

    pub fn with_cause_and_details(
        message: impl Into<String>,
        code: impl Into<String>,
        cause: Arc<dyn Error + Send + Sync>,
        details: BTreeMap<String, Value>,
    ) -> Self {
        Self {
            message: message.into(),
            code: code.into(),
            cause: Some(cause),
            details: Some(details),
        }
    }
}

pub fn iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
