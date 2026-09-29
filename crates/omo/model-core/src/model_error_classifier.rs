use crate::model_requirement_types::FallbackEntry;
use crate::provider_cache::NoopProviderCache;
use crate::provider_cache::ProviderCache;

/// Error names that halt execution and should trigger a fallback retry.
const RETRYABLE_ERROR_NAMES: &[&str] = &[
    "providermodelnotfounderror",
    "ratelimiterror",
    "modelunavailableerror",
    "providerconnectionerror",
    "authenticationerror",
];

const STOP_ERROR_NAMES: &[&str] = &[
    "quotaexceedederror",
    "insufficientcreditserror",
    "freeusagelimiterror",
];

/// Error names that are user-induced or fixable without switching models.
const NON_RETRYABLE_ERROR_NAMES: &[&str] = &[
    "messageabortederror",
    "permissiondeniederror",
    "contextlengtherror",
    "timeouterror",
    "validationerror",
    "syntaxerror",
    "usererror",
];

/// Message patterns that indicate a retryable error even without a known error name.
const RETRYABLE_MESSAGE_PATTERNS: &[&str] = &[
    "rate_limit",
    "rate limit",
    "usage_limit_reached",
    "usage limit has been reached",
    "quota",
    "all credentials for model",
    "cooling down",
    "exhausted your capacity",
    "not found",
    "unavailable",
    "insufficient",
    "too many requests",
    "over limit",
    "overloaded",
    "bad gateway",
    "bad request",
    "unknown provider",
    "provider not found",
    "model_not_supported",
    "model not supported",
    "model is not supported",
    "connection error",
    "network error",
    "timeout",
    "service unavailable",
    "internal_server_error",
    "free usage",
    "usage exceeded",
    "credit",
    "balance",
    "temporarily unavailable",
    "try again",
    "请稍后重试",
    "503",
    "502",
    "504",
    "429",
    "529",
    "selected provider is forbidden",
    "provider is forbidden",
    // Chinese retryable patterns (Zhipu, etc.)
    "频率限制",
    "请求过于频繁",
    "暂时不可用",
    "服务不可用",
    "server_error",
    "an error occurred while processing",
    "upstream request failed",
];

/// Quota/billing exhaustion patterns; these take precedence over retryable patterns.
const STOP_MESSAGE_PATTERNS: &[&str] = &[
    "quota will reset after",
    "quota exceeded",
    "free usage limit",
    "billing limit",
    "billing hard limit",
    "monthly limit",
    "plan limit",
    "subscription quota",
    "subscription limit",
    "payment required",
    "out of credits",
    "credits exhausted",
    "insufficient credits",
    "insufficient balance",
    "credit balance",
    "usage limit for this month",
    "exhausted your capacity",
    // GLM/Z.ai business error codes that indicate permanent quota/billing exhaustion
    "daily call limit",
    "daily limit",
    "usage limit reached for",
    "in arrears",
    "fair use policy",
    "recharge and try",
    "使用上限",
    "额度不足",
    "余额不足",
    "已耗尽",
];

const AUTO_RETRY_GATE_PATTERNS: &[&str] = &["rate limit", "cooling down", "credentials for model"];

fn has_provider_auto_retry_signal(message: &str) -> bool {
    message.contains("retrying in")
        && AUTO_RETRY_GATE_PATTERNS
            .iter()
            .any(|pattern| message.contains(pattern))
}

/// `{ name?; message?; statusCode? }`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErrorInfo {
    pub name: Option<String>,
    pub message: Option<String>,
    /// HTTP status code from the provider response (e.g. 429 for rate limit).
    pub status_code: Option<i64>,
}

/// Known retryable error name, or a retryable message/status that no STOP pattern overrides.
#[must_use]
pub fn is_retryable_model_error(error: &ErrorInfo) -> bool {
    if let Some(name) = error.name.as_deref().filter(|name| !name.is_empty()) {
        let error_name_lower = name.to_lowercase();
        let error_name_lower = error_name_lower.as_str();
        if NON_RETRYABLE_ERROR_NAMES.contains(&error_name_lower)
            || STOP_ERROR_NAMES.contains(&error_name_lower)
        {
            return false;
        }
        if RETRYABLE_ERROR_NAMES.contains(&error_name_lower) {
            return true;
        }
    }

    let msg = error.message.as_deref().unwrap_or_default().to_lowercase();
    if STOP_MESSAGE_PATTERNS
        .iter()
        .any(|pattern| msg.contains(pattern))
    {
        return false;
    }
    if has_provider_auto_retry_signal(&msg) {
        return true;
    }
    // 400 is excluded: it is a permanent client error.
    if matches!(error.status_code, Some(429 | 503 | 529)) {
        return true;
    }
    RETRYABLE_MESSAGE_PATTERNS
        .iter()
        .any(|pattern| msg.contains(pattern))
}

/// Whether an error should trigger a fallback retry.
#[must_use]
pub fn should_retry_error(error: &ErrorInfo) -> bool {
    is_retryable_model_error(error)
}

/// The fallback entry for this attempt, or `None` once the chain is exhausted.
#[must_use]
pub fn get_next_fallback(
    fallback_chain: &[FallbackEntry],
    attempt_count: usize,
) -> Option<&FallbackEntry> {
    fallback_chain.get(attempt_count)
}

#[must_use]
pub fn has_more_fallbacks(fallback_chain: &[FallbackEntry], attempt_count: usize) -> bool {
    attempt_count < fallback_chain.len()
}

/// Selects a provider using the default (empty) connected-providers cache.
#[must_use]
pub fn select_fallback_provider(
    providers: &[String],
    preferred_provider_id: Option<&str>,
) -> String {
    select_fallback_provider_with_cache(providers, &NoopProviderCache, preferred_provider_id)
}

/// Priority: first connected provider in preference order, then the preferred provider when
/// connected, then the entry's first provider (then the preferred one, then `opencode`).
#[must_use]
pub fn select_fallback_provider_with_cache(
    providers: &[String],
    provider_cache: &dyn ProviderCache,
    preferred_provider_id: Option<&str>,
) -> String {
    let preferred_provider_id = preferred_provider_id.filter(|id| !id.is_empty());
    if let Some(connected_providers) = provider_cache.read_connected_providers_cache() {
        let connected: Vec<String> = connected_providers
            .iter()
            .map(|p| p.to_lowercase())
            .collect();
        if let Some(provider) = providers
            .iter()
            .find(|provider| connected.contains(&provider.to_lowercase()))
        {
            return provider.clone();
        }
        if let Some(preferred) = preferred_provider_id
            && connected.contains(&preferred.to_lowercase())
        {
            return preferred.to_string();
        }
    }

    providers
        .first()
        .filter(|provider| !provider.is_empty())
        .map(String::as_str)
        .or(preferred_provider_id)
        .unwrap_or("opencode")
        .to_string()
}
