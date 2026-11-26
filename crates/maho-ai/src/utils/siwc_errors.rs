//! Exact Sign in with ChatGPT recovery policy shared by transport, assistant retry and the pool.

use crate::types::{
    ProviderDiagnostic, ProviderDiagnosticCategory as Category,
    ProviderDiagnosticEvidence as Evidence,
};
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiwcRecovery {
    UsageLimit,
    AccountDisabled,
    Auth,
    Request,
    Transient,
}

pub(crate) fn siwc_error_for_code(code: &str) -> Option<(Category, u16, SiwcRecovery)> {
    use SiwcRecovery as Recovery;
    Some(match code {
        "subscription_sharing_usage_limit_exceeded" => (Category::Quota, 429, Recovery::UsageLimit),
        "subscription_sharing_user_not_eligible" => {
            (Category::Unknown, 403, Recovery::AccountDisabled)
        }
        "subscription_sharing_usage_unavailable" | "subscription_sharing_user_unavailable" => {
            (Category::ProviderUnavailable, 503, Recovery::Transient)
        }
        "subscription_sharing_invalid_user" => (Category::Auth, 401, Recovery::Auth),
        "subscription_sharing_unsupported_capability" => {
            (Category::InvalidRequest, 400, Recovery::Request)
        }
        "subscription_sharing_route_not_supported"
        | "chatpass_v2_scope_not_authorized"
        | "chatpass_v2_invalid_authorization_context" => {
            (Category::InvalidRequest, 403, Recovery::Request)
        }
        _ => return None,
    })
}

pub fn siwc_recovery_from_diagnostic(
    diagnostic: Option<&ProviderDiagnostic>,
) -> Option<SiwcRecovery> {
    let diagnostic = diagnostic?;
    match diagnostic.evidence {
        Evidence::StructuredCode | Evidence::SiwcStructuredCode => {
            siwc_error_for_code(diagnostic.code.as_deref()?).map(|(_, _, recovery)| recovery)
        }
        Evidence::SiwcStructuredStatus => match diagnostic.http_status? {
            401 => Some(SiwcRecovery::Auth),
            403 => Some(SiwcRecovery::Request),
            503 => Some(SiwcRecovery::Transient),
            _ => None,
        },
        Evidence::StructuredStatus | Evidence::StructuredTransport => None,
    }
}

static MISSING_RESPONSES_SCOPE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)\bMissing scopes?:[^\n]*?\bapi\.responses\.write\b")
        .unwrap_or_else(|error| panic!("SIWC scope pattern: {error}"))
});

pub const SIWC_MISSING_RESPONSES_SCOPE_GUIDANCE: &str = "This ChatGPT account or plan does not have access to the Responses API through Sign in with ChatGPT (missing scope api.responses.write). Signing in again will not change that: check that plan usage is enabled for this account or workspace, or use an OpenAI API key.";

pub fn is_siwc_missing_responses_scope(text: &str) -> bool {
    MISSING_RESPONSES_SCOPE.is_match(text)
}
