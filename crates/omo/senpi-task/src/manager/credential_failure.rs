//! Failure-sensitive fallback ordering (`manager/credential-failure.ts`).
use crate::state::{ResolvedModelRecord, TaskRecord};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageLimitScope {
    Model,
    Account,
}
fn matches(pattern: &str, message: &str) -> bool {
    regex::Regex::new(pattern)
        .expect("static failure classifier")
        .is_match(message)
}
pub fn usage_limit_scope(message: &str) -> Option<UsageLimitScope> {
    if !matches(
        r"(?i)quota|usage[_ ]limit|\bhit\s+your\b[^.]*\blimit\b|\b(?:session|weekly|monthly|daily|hourly|\d+[- ]hour)\s+limit\b|limit\s+exhausted|insufficient[_ ](?:quota|credits?|balance)|credit[_ ]balance|credits?[_ ]required|billing|blocking_limit",
        message,
    ) {
        return None;
    }
    // Rust regex has no lookbehind: discard only slash-prefixed model-command hints.
    let scoped = regex::Regex::new(r"(?i)/models?\b")
        .expect("static command hint")
        .replace_all(message, "");
    Some(
        if matches(
            r"(?i)\bmodels?\b|\bpremium\b|\b(?:opus|sonnet|haiku|fable|mythos)\b",
            &scoped,
        ) {
            UsageLimitScope::Model
        } else {
            UsageLimitScope::Account
        },
    )
}
// A provider answer that no other model on the SAME provider can fix: the stored credential is
// rejected (401, an invalid key, a lapsed subscription), has expired with only a fresh /login to renew
// it ("Meta session expired (status 403). Run `/login meta` ..."), or can no longer be refreshed.
const CREDENTIAL_REJECTED: &str = r"(?i)\b401\b|unauthori[sz]ed|invalid_grant|oauth refresh failed|subscription is required|invalid api key|incorrect api key|authentication (?:failed|error)|\bthe account's oauth token was rejected\b|\bsession expired\b[^.]*\.\s*run\s+\W?/login\b";
// No credential at all for the provider (#10806): the provider is signed out, so its sibling models
// fail the same way. A "/login" hint alone is not enough.
const CREDENTIAL_MISSING: &str = r"(?i)\bno api key (?:found |provided )?for\b|\bnot logged in\b|\bno (?:[\w-]+ ){0,3}accounts?(?: configured\b|:)|\bno [\w-]+ access token\b|authentication[_ ]failed";
// A sign-in requirement: a signed-out provider unless the text scopes it to a model.
const CREDENTIAL_REQUIRED: &str =
    r"(?i)\b(?:access token|authentication) is required\b|authentication[_ ]required";

fn names_model(message: &str, model_id: Option<&str>) -> bool {
    matches(r"(?i)\bmodels?\b", message)
        || model_id.is_some_and(|id| message.to_lowercase().contains(&id.to_lowercase()))
}

fn is_missing_credential(message: &str, model_id: Option<&str>) -> bool {
    matches(CREDENTIAL_MISSING, message)
        || (matches(CREDENTIAL_REQUIRED, message) && !names_model(message, model_id))
}

pub fn is_credential_failure(message: &str, model_id: Option<&str>) -> bool {
    if matches(CREDENTIAL_REJECTED, message) || is_missing_credential(message, model_id) {
        return true;
    }
    if !matches(r"(?i)\b403\b|forbidden", message)
        || !matches(
            r"(?i)account|credential|token|api[ _-]?key|organization|subscription",
            message,
        )
    {
        return false;
    }
    !names_model(message, model_id)
}

fn provider_of(record: &TaskRecord) -> Option<&str> {
    record
        .resolved_model
        .as_ref()
        .map(|m| m.provider.as_str())
        .or_else(|| {
            record
                .model
                .split_once('/')
                .map(|(p, _)| p)
                .filter(|p| !p.is_empty())
        })
}

fn model_id_of(record: &TaskRecord) -> Option<&str> {
    record
        .resolved_model
        .as_ref()
        .map(|m| m.model_id.as_str())
        .or_else(|| {
            record
                .model
                .split_once('/')
                .filter(|(p, _)| !p.is_empty())
                .map(|(_, m)| m)
        })
}

/// The terminal error text: a credential failure names the provider and how to restore it, and a
/// spent usage limit says the task stopped on it (and, when nothing was left, that no other model
/// could take over).
pub fn terminal_failure_message(record: Option<&TaskRecord>, failure_message: &str) -> String {
    let Some(record) = record else {
        return failure_message.to_string();
    };
    let Some(provider) = provider_of(record) else {
        return failure_message.to_string();
    };
    if is_credential_failure(failure_message, model_id_of(record)) {
        let missing = !matches(CREDENTIAL_REJECTED, failure_message)
            && is_missing_credential(failure_message, model_id_of(record));
        let cause = if missing {
            format!(
                "No credentials for {provider} were found; sign in to {provider} (Provider authentication settings on the desktop, /login {provider} in an interactive session) or add its API key"
            )
        } else {
            format!(
                "Credentials for {provider} were rejected; re-authenticate {provider} (Provider authentication settings on the desktop, /login {provider} in an interactive session) or re-add its API key"
            )
        };
        return format!(
            "{failure_message}\n{cause}, or pin this category to another provider in omo.json."
        );
    }
    if usage_limit_scope(failure_message).is_none() {
        return failure_message.to_string();
    }
    let model = match &record.resolved_model {
        Some(resolved) => format!("{provider}/{}", resolved.model_id),
        None => record.model.clone(),
    };
    let exhausted = if runtime_fallback_candidates(record, failure_message)
        .remaining
        .is_empty()
    {
        " No other model in its fallback chain could take over."
    } else {
        ""
    };
    format!(
        "{failure_message}\n{model} reached its usage limit, so this task stopped.{exhausted} Retry once the limit resets, or point this category at another provider or model in omo.json."
    )
}
#[derive(Debug, Default)]
pub struct RuntimeFallbackCandidates {
    pub remaining: Vec<ResolvedModelRecord>,
    pub skipped: Vec<ResolvedModelRecord>,
    pub retained: Vec<ResolvedModelRecord>,
    pub limit: Option<UsageLimitScope>,
}
impl RuntimeFallbackCandidates {
    pub fn chain_after_hop(&self) -> Vec<ResolvedModelRecord> {
        self.remaining
            .iter()
            .skip(1)
            .chain(self.retained.iter())
            .cloned()
            .collect()
    }
}
pub fn runtime_fallback_candidates(
    record: &TaskRecord,
    message: &str,
) -> RuntimeFallbackCandidates {
    let provider = record
        .resolved_model
        .as_ref()
        .map(|m| m.provider.as_str())
        .or_else(|| record.model.split_once('/').map(|(p, _)| p));
    let model_id = record
        .resolved_model
        .as_ref()
        .map(|m| m.model_id.as_str())
        .or_else(|| record.model.split_once('/').map(|(_, m)| m));
    let credential = is_credential_failure(message, model_id);
    let limit = usage_limit_scope(message);
    let is_parent = |m: &ResolvedModelRecord| {
        record.parent_model.as_deref() == Some(format!("{}/{}", m.provider, m.model_id).as_str())
    };
    let mut result = RuntimeFallbackCandidates {
        limit,
        ..Default::default()
    };
    for model in record.fallback_models.iter().flatten() {
        if limit.is_none() && !credential && is_parent(model) {
            result.retained.push(model.clone());
        } else if (credential && provider == Some(model.provider.as_str()))
            || (limit == Some(UsageLimitScope::Account)
                && provider == Some(model.provider.as_str())
                && is_parent(model))
        {
            result.skipped.push(model.clone());
        } else {
            result.remaining.push(model.clone());
        }
    }
    if limit == Some(UsageLimitScope::Account) && !credential {
        result
            .remaining
            .sort_by_key(|model| provider == Some(model.provider.as_str()));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_hint_does_not_name_a_model_but_family_and_premium_do() {
        assert_eq!(
            usage_limit_scope("Weekly limit exhausted. Switch with /model"),
            Some(UsageLimitScope::Account)
        );
        assert_eq!(
            usage_limit_scope("Fable weekly limit exhausted"),
            Some(UsageLimitScope::Model)
        );
        assert_eq!(
            usage_limit_scope("Premium usage limit"),
            Some(UsageLimitScope::Model)
        );
        assert_eq!(usage_limit_scope("temporary outage"), None);
    }
    #[test]
    fn forbidden_requires_account_scope_and_preserves_model_access_failures() {
        assert!(is_credential_failure(
            "403 subscription expired",
            Some("model-a")
        ));
        assert!(!is_credential_failure(
            "403 account cannot access model",
            Some("model-a")
        ));
        assert!(!is_credential_failure(
            "403 account cannot access model-a",
            Some("model-a")
        ));
        assert!(is_credential_failure("401 unauthorized", None));
    }

    // omo v5.1.52 credential-failure.test.ts: every wording senpi emits for a missing credential (#10806).
    #[test]
    fn missing_credentials_are_provider_wide_credential_failures() {
        for message in [
            "No API key found for openai.\n\nUse /login to log into a provider via OAuth or API key. See:\n  ~/docs/providers.md",
            "No API key found for the selected model.\n\nUse /login to log into a provider via OAuth or API key.",
            "No API key found for \"openai\"",
            "No API key for provider: openai",
            "No API key provided for provider \"openai\"",
            "No API key for openai/gpt-6-luna-fast",
            "authentication_failed: No Anthropic Subscription accounts configured for the managed lane; run /login anthropic-subscription or set CLAUDE_CODE_OAUTH_TOKEN",
            "no accounts: run /login cursor-cli-oauth",
            "Cursor access token is required; run /login cursor",
            "Radius authentication is required; start Pi and run /login radius, then retry",
            "Not logged in \u{00b7} Please run /login",
            "Radius authentication is required",
            "Authentication required. Run /login openai",
            "authentication_required: sign in to openai",
            "No GitLab access token. Run /login gitlab-duo or set GITLAB_TOKEN",
            "The account's OAuth token was rejected. Re-run /login anthropic-subscription to refresh it, or remove the account with /claude-account remove <name>.",
            "Meta session expired (status 403). Run `/login meta` to sign in again.",
            "Meta session expired (status 401). Run `/login meta` to sign in again.",
            "Authentication failed for \"anthropic\". Credentials may have expired or network is unavailable. Run '/login anthropic' to re-authenticate.",
        ] {
            assert!(
                is_credential_failure(message, Some("gpt-6-luna-fast")),
                "{message}"
            );
        }
    }

    #[test]
    fn login_guidance_expiries_and_model_scoped_requirements_are_not_credential_failures() {
        for message in [
            "GitHub Copilot sent this account to a different API host (HTTP 421 Misdirected Request): Business and Enterprise accounts are served from their own host, which senpi reads from the Copilot token. Run /login github-copilot so senpi stores the account's endpoint, and report it with the GitHub request id if it persists.",
            "auth stale - run /login",
            "Access token is required for this model",
            "Authentication is required for this model",
            "Authentication required for this model",
            "authentication_required: this model needs a separate sign-in",
            "Access token is required for gpt-6-luna-fast",
            "Authentication required for gpt-6-luna-fast",
            "authentication_required: gpt-6-luna-fast needs a separate sign-in",
            "Meta device authorization expired. Please restart login.",
            "xAI device code expired",
            "MCP server docs session expired; reinitialize retry also expired; run /mcp reconnect docs",
            "MCP server docs access token was rejected",
            "This session is 180k tokens and its cache has expired.",
        ] {
            assert!(
                !is_credential_failure(message, Some("gpt-6-luna-fast")),
                "{message}"
            );
        }
        assert!(!is_credential_failure(
            "No model selected.\n\nUse /login to log into a provider via OAuth or API key.\n\nThen use /model to select a model.",
            None
        ));
    }

    fn record_on(provider: &str, model_id: &str) -> TaskRecord {
        let mut record = crate::test_support::base_record("task-1", "parent-1");
        record.model = format!("{provider}/{model_id}");
        record.resolved_model = Some(ResolvedModelRecord {
            source: crate::state::ResolvedModelSource::Category,
            provider: provider.into(),
            model_id: model_id.into(),
            display: format!("{provider}/{model_id}"),
            variant: None,
            reasoning_effort: None,
            reasoning: None,
            service_tier: None,
        });
        record
    }

    #[test]
    fn terminal_message_says_missing_or_rejected() {
        let record = record_on("openai", "gpt-6-luna-fast");
        let missing = terminal_failure_message(Some(&record), "No API key found for openai.");
        assert!(missing.contains("\nNo credentials for openai were found; sign in to openai"));
        assert!(missing.ends_with(", or pin this category to another provider in omo.json."));
        let rejected = terminal_failure_message(Some(&record), "401 unauthorized");
        assert!(
            rejected.contains("\nCredentials for openai were rejected; re-authenticate openai")
        );
        assert_eq!(terminal_failure_message(None, "401"), "401");
        assert_eq!(
            terminal_failure_message(Some(&record), "500: overloaded"),
            "500: overloaded"
        );
    }
}
