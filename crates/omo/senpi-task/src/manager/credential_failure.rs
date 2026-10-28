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
pub fn is_credential_failure(message: &str, model_id: Option<&str>) -> bool {
    matches(
        r"(?i)\b401\b|unauthori[sz]ed|invalid_grant|oauth refresh failed|subscription is required|invalid api key|incorrect api key|authentication (?:failed|error)",
        message,
    ) || (matches(r"(?i)\b403\b|forbidden", message)
        && matches(
            r"(?i)account|credential|token|api[ _-]?key|organization|subscription",
            message,
        )
        && !matches(r"(?i)\bmodels?\b", message)
        && model_id.is_none_or(|id| !message.to_lowercase().contains(&id.to_lowercase())))
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
}
