//! Port of senpi packages/ai/src/utils/provider-error.ts.

use crate::types::{AssistantErrorCause, AssistantMessage, StopReason};
use regex::Regex;
use std::sync::LazyLock;

pub use super::secret_redaction::{redact_credential_shapes, redact_secrets};

// A route that does not serve a configured model id: the OpenAI-compatible `model_not_found` code
// and OpenAI's "The model `<id>` does not exist" prose, a gateway's "unknown provider for model",
// Anthropic's not_found_error whose message names the model ("model: <id>"; any other
// not_found_error is not one), and Google's "models/<id> is not found". Every quantifier is
// bounded, so a long run never makes a match attempt expensive. `\w` is spelled as the ASCII class
// JS uses, and `\b` is the ASCII word boundary `(?-u:\b)` JS uses.
static MODEL_CONFIG_ERROR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)(?-u:\b)model_not_found(?-u:\b)",
        r"|(?-u:\b)unknown provider for model(?-u:\b)",
        r"|(?-u:\b)the model\s{1,8}[`'\x22]?[A-Za-z0-9_.:/-]{1,256}[`'\x22]?\s{1,8}does not exist(?-u:\b)",
        r"|(?-u:\b)not_found_error(?-u:\b)[^}]{0,120}?(?-u:\b)model:\s{0,8}[A-Za-z0-9_.:/-]",
        r"|(?-u:\b)models/[A-Za-z0-9_.:-]{1,256} is not found(?-u:\b)",
    ))
    .unwrap_or_else(|error| panic!("MODEL_CONFIG_ERROR is a valid pattern: {error}"))
});

/// True when provider text says the route does not serve the configured model id (senpi#4282).
pub fn is_model_config_error(error_message: Option<&str>) -> bool {
    error_message.is_some_and(|message| MODEL_CONFIG_ERROR.is_match(message))
}

/// The single point where a provider error body enters a message: classify a model configuration
/// error on the RAW text first (carried as `error_cause`), then keep only the redacted text for
/// every provider error. Idempotent; called for every message a provider stream publishes, so no
/// partial, event or final result carries a raw body.
pub fn sanitize_provider_error_message(message: &mut AssistantMessage) {
    let Some(raw) = message.error_message.as_deref() else {
        return;
    };
    if raw.is_empty() {
        return;
    }
    if message.stop_reason == StopReason::Error
        && !matches!(
            message.error_cause,
            Some(AssistantErrorCause::ModelConfig { .. })
        )
        && is_model_config_error(Some(raw))
    {
        message.error_cause = Some(AssistantErrorCause::ModelConfig {
            model: format!("{}/{}", message.provider, message.model),
        });
    }
    let redacted = redact_secrets(raw);
    if redacted != raw {
        message.error_message = Some(redacted);
    }
}

#[cfg(test)]
#[path = "provider_error_tests.rs"]
pub(crate) mod tests;
