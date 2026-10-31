//! Port of senpi packages/coding-agent/src/core/provider-failure.ts.
//!
//! Structured failure of a turn that ended with a provider error. Built only from the adapter-minted
//! `ProviderDiagnostic` and the runtime's typed served-credential mark; message text is never read. Every category, `unknown` included, carries a
//! reason and an action.

use maho_agent::types::AgentMessage;
use maho_ai::types::{
    Message, ProviderDiagnostic, ProviderDiagnosticCategory as Category, StopReason,
    sanitize_provider_diagnostic,
};

/// senpi `ProviderFailureCategory`: a diagnostic category, or `request_refused` — an `auth` failure on a
/// credential that served a response moments before, so the provider refused this request rather than the
/// key (senpi#2491).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderFailureCategory {
    Diagnostic(Category),
    RequestRefused,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFailure {
    pub category: ProviderFailureCategory,
    /// Provider id of the failing request.
    pub provider: String,
    /// Model id of the failing request (senpi#4328).
    pub model: Option<String>,
    /// The provider and model the user selected, present (both) only when the failing request ran on another
    /// one: a fallback rung, or the route a virtual model picked (senpi#4328).
    pub selected_provider: Option<String>,
    pub selected_model: Option<String>,
    pub http_status: Option<u16>,
    pub code: Option<String>,
    /// When the limit lifts (epoch ms), for a `quota` or `rate_limit` failure whose provider named it.
    pub resets_at: Option<i64>,
    pub reason: String,
    pub action: String,
}

fn known_detail(category: ProviderFailureCategory) -> Option<(&'static str, &'static str)> {
    let category = match category {
        ProviderFailureCategory::RequestRefused => {
            return Some((
                "the provider refused this request, but the same key worked moments ago, so it may still be valid",
                "send the message again or choose another model; if it keeps failing, re-authenticate",
            ));
        }
        ProviderFailureCategory::Diagnostic(category) => category,
    };
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

/// An `auth` failure on a credential that served a response moments before: the request was refused, not the key.
pub fn is_request_level_refusal(message: &maho_ai::types::AssistantMessage) -> bool {
    message.credential_served_recently == Some(true)
        && message
            .provider_diagnostic
            .as_ref()
            .and_then(sanitize_provider_diagnostic)
            .is_some_and(|diagnostic| diagnostic.category == Category::Auth)
}

/// The provider and model a turn was meant to run on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedModel {
    pub provider: String,
    pub model: String,
}

/// The model the user selected for this turn: the original of an active fallback chain, else the session
/// model (which stays the virtual model when one routes the request).
pub fn selected_model_of(
    original_selector: Option<&str>,
    models: &[maho_ai::types::Model],
    session_model: Option<&maho_ai::types::Model>,
) -> Option<SelectedModel> {
    if let Some(original) = original_selector
        .and_then(|raw| crate::retry_fallback::chains::parse_fallback_selector(raw, models))
    {
        return Some(SelectedModel {
            provider: original.provider,
            model: original.id,
        });
    }
    session_model.map(|model| SelectedModel {
        provider: model.provider.clone(),
        model: model.id.clone(),
    })
}

/// senpi `describeProviderFailure(diagnostic, provider, { model, requestRefused })`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DescribeOptions<'a> {
    pub model: Option<&'a str>,
    pub request_refused: bool,
}

pub fn describe_provider_failure(
    diagnostic: Option<&ProviderDiagnostic>,
    provider: &str,
    options: DescribeOptions<'_>,
) -> ProviderFailure {
    let category = if options.request_refused {
        ProviderFailureCategory::RequestRefused
    } else {
        ProviderFailureCategory::Diagnostic(
            diagnostic.map_or(Category::Unknown, |diagnostic| diagnostic.category),
        )
    };
    let (mut reason, mut action) = match known_detail(category) {
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
    if category == ProviderFailureCategory::RequestRefused {
        action = format!("{action} {provider}");
    }
    if let Some(iso) = resets_at.and_then(iso_string) {
        reason = format!("{reason} until {iso}");
    }
    ProviderFailure {
        category,
        provider: provider.to_owned(),
        model: options.model.map(str::to_owned),
        selected_provider: None,
        selected_model: None,
        http_status: diagnostic.and_then(|diagnostic| diagnostic.http_status),
        code: diagnostic.and_then(|diagnostic| diagnostic.code.clone()),
        resets_at,
        reason,
        action,
    }
}

/// The failure of a turn whose last assistant message ended with `stop_reason: error`. `None` on success
/// and on abort.
pub fn provider_failure_of(
    messages: &[AgentMessage],
    selected: Option<&SelectedModel>,
) -> Option<ProviderFailure> {
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
    let mut failure = describe_provider_failure(
        sanitized.as_ref(),
        &last.provider,
        DescribeOptions {
            model: Some(&last.model),
            request_refused: is_request_level_refusal(last),
        },
    );
    if let Some(selected) = selected
        && (selected.provider != last.provider || selected.model != last.model)
    {
        failure.selected_provider = Some(selected.provider.clone());
        failure.selected_model = Some(selected.model.clone());
    }
    Some(failure)
}

pub fn format_provider_failure(failure: &ProviderFailure) -> String {
    let route = match &failure.model {
        Some(model) => format!("{model} on {}", failure.provider),
        None => failure.provider.clone(),
    };
    let selected = match (&failure.selected_provider, &failure.selected_model) {
        (Some(provider), Some(model)) => format!(" (selected: {model} on {provider})"),
        _ => String::new(),
    };
    format!(
        "{route}{selected} failed: {}. Action: {}.",
        failure.reason, failure.action
    )
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
            DescribeOptions::default(),
        );
        assert_eq!(
            failure.reason,
            "the provider account is out of quota until 2023-11-14T22:13:20.000Z"
        );
        assert_eq!(failure.resets_at, Some(1_700_000_000_000));
    }

    #[test]
    fn an_unclassified_failure_names_the_provider_and_identifier() {
        let failure = describe_provider_failure(None, "x", DescribeOptions::default());
        assert_eq!(
            failure.reason,
            "the provider returned an error that could not be classified (x, no code)"
        );
        assert_eq!(
            format_provider_failure(&describe_provider_failure(
                Some(&diagnostic(Category::Auth, None)),
                "x",
                DescribeOptions::default(),
            )),
            "x failed: the provider rejected the credentials. Action: re-authenticate the provider."
        );
    }

    /// senpi test/suite/regressions/4328-provider-failure-route.test.ts: the failing route and, when it
    /// differs, the selected model are named.
    #[test]
    fn the_failure_names_the_failing_route_and_the_selected_model() {
        let mut failure = describe_provider_failure(
            Some(&diagnostic(Category::RateLimit, None)),
            "openrouter",
            DescribeOptions {
                model: Some("gpt-x"),
                request_refused: false,
            },
        );
        failure.selected_provider = Some("anthropic".into());
        failure.selected_model = Some("claude".into());
        assert_eq!(
            format_provider_failure(&failure),
            "gpt-x on openrouter (selected: claude on anthropic) failed: the provider is rate limiting requests. Action: retry later or set a fallback model."
        );
    }

    /// senpi test/suite/regressions/2491-request-refused.test.ts: a refusal names the provider in its action.
    #[test]
    fn a_request_refusal_keeps_the_key_and_names_the_provider() {
        let failure = describe_provider_failure(
            Some(&diagnostic(Category::Auth, None)),
            "gw",
            DescribeOptions {
                model: None,
                request_refused: true,
            },
        );
        assert_eq!(failure.category, ProviderFailureCategory::RequestRefused);
        assert_eq!(
            failure.action,
            "send the message again or choose another model; if it keeps failing, re-authenticate gw"
        );
    }
}
