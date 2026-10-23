//! Port of senpi packages/coding-agent/src/core/provider-failure.ts.
//!
//! Structured failure of a turn that ended with a provider error. Built only from the adapter-minted
//! `ProviderDiagnostic`; message text is never read. Every category, `unknown` included, carries a
//! reason and an action.

use maho_agent::types::AgentMessage;
use maho_ai::types::{
    Message, ProviderDiagnostic, ProviderDiagnosticCategory as Category, StopReason,
    sanitize_provider_diagnostic,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFailure {
    pub category: Category,
    pub provider: String,
    pub http_status: Option<u16>,
    pub code: Option<String>,
    /// When the limit lifts (epoch ms), for a `quota` or `rate_limit` failure whose provider named it.
    pub resets_at: Option<i64>,
    pub reason: String,
    pub action: String,
}

fn known_detail(category: Category) -> Option<(&'static str, &'static str)> {
    Some(match category {
        Category::Auth => (
            "the provider rejected the credentials",
            "re-authenticate the provider",
        ),
        Category::RateLimit => (
            "the provider is rate limiting requests",
            "retry later or set a fallback model",
        ),
        Category::Quota => (
            "the provider account is out of quota",
            "check the plan or credits",
        ),
        Category::ContextLimit => (
            "the conversation is larger than the model's context",
            "compact or shorten the conversation",
        ),
        Category::InvalidRequest => (
            "the provider rejected the request as invalid",
            "report it with the code",
        ),
        Category::ProviderUnavailable => {
            ("the provider is unavailable", "retry or switch provider")
        }
        Category::Network => (
            "the connection to the provider failed",
            "check connectivity",
        ),
        Category::Unknown => return None,
    })
}

fn identifier(diagnostic: Option<&ProviderDiagnostic>) -> String {
    if let Some(code) = diagnostic.and_then(|diagnostic| diagnostic.code.as_deref()) {
        return format!("code {code}");
    }
    if let Some(status) = diagnostic.and_then(|diagnostic| diagnostic.http_status) {
        return format!("status {status}");
    }
    "no code".to_owned()
}

/// `toISOString()` of an epoch-ms instant; `None` outside the representable range.
fn iso_string(epoch_ms: i64) -> Option<String> {
    let instant = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(epoch_ms)?;
    Some(instant.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

pub fn describe_provider_failure(
    diagnostic: Option<&ProviderDiagnostic>,
    provider: &str,
) -> ProviderFailure {
    let category = diagnostic.map_or(Category::Unknown, |diagnostic| diagnostic.category);
    let (mut reason, action) = match known_detail(category) {
        Some((reason, action)) => (reason.to_owned(), action.to_owned()),
        None => (
            format!(
                "the provider returned an error that could not be classified ({provider}, {})",
                identifier(diagnostic)
            ),
            "retry once; if it repeats, switch the model or report the code".to_owned(),
        ),
    };
    let resets_at = diagnostic.and_then(|diagnostic| diagnostic.resets_at);
    if let Some(iso) = resets_at.and_then(iso_string) {
        reason = format!("{reason} until {iso}");
    }
    ProviderFailure {
        category,
        provider: provider.to_owned(),
        http_status: diagnostic.and_then(|diagnostic| diagnostic.http_status),
        code: diagnostic.and_then(|diagnostic| diagnostic.code.clone()),
        resets_at,
        reason,
        action,
    }
}

/// The failure of a turn whose last assistant message ended with `stop_reason: error`. `None` on success
/// and on abort.
pub fn provider_failure_of(messages: &[AgentMessage]) -> Option<ProviderFailure> {
    let last = messages.iter().rev().find_map(|message| match message {
        AgentMessage::Llm(Message::Assistant(assistant)) => Some(assistant),
        _ => None,
    })?;
    if last.stop_reason != StopReason::Error {
        return None;
    }
    let sanitized = last
        .provider_diagnostic
        .as_ref()
        .and_then(sanitize_provider_diagnostic);
    Some(describe_provider_failure(
        sanitized.as_ref(),
        &last.provider,
    ))
}

pub fn format_provider_failure(failure: &ProviderFailure) -> String {
    let mut characters = failure.reason.chars();
    let reason = characters.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(characters).collect()
    });
    format!("{reason}. Action: {}.", failure.action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ai::types::ProviderDiagnosticEvidence;

    fn diagnostic(category: Category, resets_at: Option<i64>) -> ProviderDiagnostic {
        ProviderDiagnostic {
            category,
            http_status: Some(429),
            code: None,
            evidence: ProviderDiagnosticEvidence::StructuredStatus,
            resets_at,
        }
    }

    #[test]
    fn a_named_reset_extends_the_reason_with_an_iso_instant() {
        let failure = describe_provider_failure(
            Some(&diagnostic(Category::Quota, Some(1_700_000_000_000))),
            "anthropic",
        );
        assert_eq!(
            failure.reason,
            "the provider account is out of quota until 2023-11-14T22:13:20.000Z"
        );
        assert_eq!(failure.resets_at, Some(1_700_000_000_000));
    }

    #[test]
    fn an_unclassified_failure_names_the_provider_and_identifier() {
        let failure = describe_provider_failure(None, "x");
        assert_eq!(
            failure.reason,
            "the provider returned an error that could not be classified (x, no code)"
        );
        assert_eq!(
            format_provider_failure(&describe_provider_failure(
                Some(&diagnostic(Category::Auth, None)),
                "x"
            )),
            "The provider rejected the credentials. Action: re-authenticate the provider."
        );
    }
}
