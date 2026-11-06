use maho_ai::types::{AssistantErrorCause, AssistantMessage, UsageLimitScope};
use serde_json::Value;

pub fn usage_limit_scope(message: &AssistantMessage) -> Option<UsageLimitScope> {
    // Only a usage_limit cause carries a scope; a model_config cause has none (senpi c2bef0ffa4).
    message.error_cause.as_ref().and_then(|cause| match cause {
        AssistantErrorCause::UsageLimit { scope } => Some(*scope),
        _ => None,
    })
}

// Subscription limit prose that carries no quota/billing keyword, e.g. Claude's "You've hit your session
// limit · resets 3pm" and the SDK terminal reasons.
static SUBSCRIPTION_LIMIT_PATTERN: std::sync::LazyLock<Option<regex::Regex>> =
    std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)\bhit\s+your\b[^.]*\blimit\b|\b(?:session|weekly|monthly|daily|hourly|\d+[- ]hour|usage)\s+limit\b|\bblocking_limit\b|\brapid_refill_breaker\b").ok()
    });
// A plan cap that tells the user to switch models (senpi#2660). An ordinary "rate limit exceeded" stays transient.
static PLAN_MODEL_LIMIT_PATTERN: std::sync::LazyLock<Option<regex::Regex>> =
    std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)\breached\b[^.]*\b(?:free|plan|tier|model)\b[^.]*\blimit\b[\s\S]*\bswitch to (?:a |an )?(?:different|another) model\b").ok()
    });
static MODEL_SCOPE_PATTERN: std::sync::LazyLock<Option<regex::Regex>> =
    std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)\bmodels?\b|\bpremium\b|\b(?:opus|sonnet|haiku|fable|mythos)\b")
            .ok()
    });

fn pattern_matches(pattern: &std::sync::LazyLock<Option<regex::Regex>>, text: &str) -> bool {
    pattern
        .as_ref()
        .is_some_and(|pattern| pattern.is_match(text))
}

/// senpi `usageLimitScope(errorMessage)`: who a usage limit named only in prose binds. `Model`: the provider
/// refused this model only; `Account`: the credential itself is spent.
pub fn usage_limit_scope_text(error_message: Option<&str>) -> Option<UsageLimitScope> {
    let error_message = error_message?;
    // Claude Code's "You've reached your Fable limit. Switch to another model" names the family it binds.
    if crate::credential_pool::model_scope::rate_limit_model_family(error_message).is_some() {
        return Some(UsageLimitScope::Model);
    }
    let limited = super::circuit_probes::is_quota_exhaustion_message(Some(error_message))
        || super::billing::is_billing_error_message(Some(error_message))
        || pattern_matches(&SUBSCRIPTION_LIMIT_PATTERN, error_message)
        || pattern_matches(&PLAN_MODEL_LIMIT_PATTERN, error_message);
    if !limited {
        return None;
    }
    Some(if pattern_matches(&MODEL_SCOPE_PATTERN, error_message) {
        UsageLimitScope::Model
    } else {
        UsageLimitScope::Account
    })
}

/// The notice clause naming what hit the limit, or `None` when no usage limit caused the switch.
pub fn usage_limit_cause(from: &str, limit: Option<UsageLimitScope>) -> Option<String> {
    match limit? {
        UsageLimitScope::Model => Some(format!("{from} hit its usage limit")),
        UsageLimitScope::Account => {
            let provider = from
                .split_once('/')
                .map(|(provider, _)| provider)
                .filter(|p| !p.is_empty())
                .unwrap_or(from);
            Some(format!(
                "the {provider} account hit its usage limit, so its other models were skipped"
            ))
        }
    }
}

pub fn describe_usage_limit_message(message: &AssistantMessage) -> AssistantMessage {
    let mut projected = message.clone();
    match usage_limit_scope(message) {
        Some(UsageLimitScope::Account) => {
            if let Some(raw) = message
                .error_message
                .as_deref()
                .filter(|raw| !raw.is_empty())
            {
                projected.error_message = Some(describe_account_limit(&message.provider, raw));
            }
        }
        Some(UsageLimitScope::Model) | None => {}
    }
    projected
}

/// The account-limit advice for `provider`, carrying the reset time named in `raw` when there is one.
fn describe_account_limit(provider: &str, raw: &str) -> String {
    let raw =
        maho_ai::utils::provider_failure_description::strip_turn_retry_suppression_prefix(raw);
    if raw.contains("To keep going now, ") {
        raw.rsplit_once(" (retry-after-ms: ")
            .filter(|(_, suffix)| {
                suffix.strip_suffix(')').is_some_and(|millis| {
                    !millis.is_empty() && millis.bytes().all(|byte| byte.is_ascii_digit())
                })
            })
            .map_or_else(|| raw.clone(), |(message, _)| message.to_owned())
    } else {
        let reset = raw.split_whitespace().collect::<Vec<_>>();
        let reset = reset
            .iter()
            .position(|word| {
                word.eq_ignore_ascii_case("reset") || word.eq_ignore_ascii_case("resets")
            })
            .map(|index| reset[index.saturating_add(1)..].join(" "))
            .map(|reset| {
                reset
                    .split(['"', '}', '\n'])
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches(['.', ';'])
                    .trim()
                    .to_owned()
            })
            .filter(|reset| !reset.is_empty())
            .map(|reset| format!(" (resets {reset})"))
            .unwrap_or_default();
        format!(
            "The {provider} account usage limit has been reached{reset}. Switch to another provider with /model, or configure another provider in retry.fallbackChains."
        )
    }
}

/// senpi `AccountUsageLimitError`: a spent account with nowhere to route, phrased as the account advice.
pub fn account_usage_limit_error(provider: &str, raw: Option<&str>) -> String {
    describe_account_limit(
        provider,
        raw.filter(|raw| !raw.is_empty())
            .unwrap_or("usage limit reached"),
    )
}

pub fn describe_usage_limit_entry(mut entry: Value) -> Value {
    if entry["type"] == "message"
        && entry["message"]["role"] == "assistant"
        && let Ok(message) = serde_json::from_value::<AssistantMessage>(entry["message"].clone())
    {
        let projected = describe_usage_limit_message(&message);
        if projected.error_message != message.error_message {
            entry["message"]["errorMessage"] =
                projected.error_message.map_or(Value::Null, Value::String);
        }
    }
    entry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_usage_limit_prose_by_scope() {
        assert_eq!(
            usage_limit_scope_text(Some("You've hit your session limit · resets 3pm")),
            Some(UsageLimitScope::Account)
        );
        assert_eq!(
            usage_limit_scope_text(Some("insufficient_quota")),
            Some(UsageLimitScope::Account)
        );
        assert_eq!(
            usage_limit_scope_text(Some("Premium models are not included: usage limit reached")),
            Some(UsageLimitScope::Model)
        );
        assert_eq!(
            usage_limit_scope_text(Some(
                "Reached free model rate limit. Upgrade, or switch to a different model."
            )),
            Some(UsageLimitScope::Model)
        );
        assert_eq!(
            usage_limit_scope_text(Some("429 rate limit exceeded")),
            None
        );
        assert_eq!(usage_limit_scope_text(None), None);
    }

    #[test]
    fn names_what_hit_the_limit() {
        assert_eq!(
            usage_limit_cause("openai/gpt-5", Some(UsageLimitScope::Model)).as_deref(),
            Some("openai/gpt-5 hit its usage limit")
        );
        assert_eq!(
            usage_limit_cause("openai/gpt-5", Some(UsageLimitScope::Account)).as_deref(),
            Some("the openai account hit its usage limit, so its other models were skipped")
        );
        assert_eq!(usage_limit_cause("openai/gpt-5", None), None);
    }

    #[test]
    fn usage_projection_preserves_cause_and_identity_without_classifying_text() {
        // Given
        let mut message = maho_ai::providers::faux::faux_assistant_message("", Default::default());
        message.error_message = Some("diagnostic".into());
        message.error_cause = Some(AssistantErrorCause::UsageLimit {
            scope: UsageLimitScope::Account,
        });
        let original = serde_json::json!({"type":"message", "id":"entry", "parentId":"parent", "message":message});
        // When
        let projected = describe_usage_limit_entry(original.clone());
        // Then
        assert_eq!(projected["id"], original["id"]);
        assert_eq!(projected["parentId"], original["parentId"]);
        assert_eq!(
            projected["message"]["errorCause"],
            original["message"]["errorCause"]
        );
        assert_ne!(
            projected["message"]["errorMessage"],
            original["message"]["errorMessage"]
        );
        message.error_cause = None;
        message.error_message = Some("usage_limit_reached".into());
        assert!(usage_limit_scope(&message).is_none());
        assert_eq!(describe_usage_limit_message(&message), message);
        message.error_cause = Some(AssistantErrorCause::UsageLimit {
            scope: UsageLimitScope::Model,
        });
        assert_eq!(describe_usage_limit_message(&message), message);
    }

    /// senpi c2bef0ffa4: a model_config cause is not a usage limit and yields no scope.
    #[test]
    fn a_model_config_cause_yields_no_scope() {
        let mut message = maho_ai::providers::faux::faux_assistant_message("", Default::default());
        message.error_message = Some("unknown model".into());
        message.error_cause = Some(AssistantErrorCause::ModelConfig { model: "m".into() });
        assert!(usage_limit_scope(&message).is_none());
        assert_eq!(describe_usage_limit_message(&message), message);
    }
}
