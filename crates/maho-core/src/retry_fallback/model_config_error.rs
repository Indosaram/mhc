//! Port of senpi packages/coding-agent/src/core/retry-fallback/model-config-error.ts (senpi#4282).

use maho_ai::types::{AssistantErrorCause, AssistantMessage};
use std::collections::HashSet;

const EXCERPT_LIMIT: usize = 500;

/// The `model_config_error` session event.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelConfigErrorEvent {
    pub model: String,
    pub provider: String,
    pub error_excerpt: String,
}

/// A model configuration error is never retried or fallen back: another model would answer a request the
/// caller did not make. The provider stream classified the raw body and redacted it, so only the typed cause
/// is read here, never the redacted text.
pub fn is_model_config_failure(message: &AssistantMessage) -> bool {
    matches!(
        message.error_cause,
        Some(AssistantErrorCause::ModelConfig { .. })
    )
}

/// senpi `createModelConfigErrorReporter`: emits the event and logs a warning once per model and session.
#[derive(Debug, Default)]
pub struct ModelConfigErrorReporter {
    reported: HashSet<String>,
}

impl ModelConfigErrorReporter {
    /// `None` when this model already reported in this session; else the event and the warning to log.
    pub fn report(
        &mut self,
        message: &AssistantMessage,
        session_id: &str,
    ) -> Option<ModelConfigErrorEvent> {
        let model = format!("{}/{}", message.provider, message.model);
        if !self.reported.insert(format!("{session_id}\u{0}{model}")) {
            return None;
        }
        let error_message = message.error_message.as_deref().unwrap_or("");
        Some(ModelConfigErrorEvent {
            model,
            provider: message.provider.clone(),
            error_excerpt: error_message.chars().take(EXCERPT_LIMIT).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank() -> AssistantMessage {
        serde_json::from_value(serde_json::json!({
            "content": [], "api": "faux", "provider": "faux", "model": "faux-1",
            "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } },
            "stopReason": "stop", "timestamp": 0
        }))
        .expect("assistant")
    }

    /// senpi test/suite/regressions/4282-model-config-error.test.ts: reported once per model and session.
    #[test]
    fn a_model_config_failure_reports_once_per_model_and_session() {
        let mut message = blank();
        message.provider = "p".into();
        message.model = "m".into();
        message.error_message = Some("x".repeat(600));
        assert!(!is_model_config_failure(&message));
        message.error_cause = Some(AssistantErrorCause::ModelConfig { model: "m".into() });
        assert!(is_model_config_failure(&message));
        let mut reporter = ModelConfigErrorReporter::default();
        let event = reporter.report(&message, "s").expect("first");
        assert_eq!(event.model, "p/m");
        assert_eq!(event.error_excerpt.len(), 500);
        assert!(reporter.report(&message, "s").is_none());
        assert!(reporter.report(&message, "s2").is_some());
    }
}
