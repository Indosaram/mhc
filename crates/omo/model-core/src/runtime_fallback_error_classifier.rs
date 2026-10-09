use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

pub use crate::runtime_fallback_auto_retry_signal::RuntimeFallbackAutoRetrySignal;
pub use crate::runtime_fallback_auto_retry_signal::extract_runtime_fallback_auto_retry_signal;
pub use crate::runtime_fallback_error_shape::get_runtime_fallback_error_message;
pub use crate::runtime_fallback_error_shape::get_runtime_fallback_error_name;
pub use crate::runtime_fallback_error_shape::get_runtime_fallback_retryable_signal;
pub use crate::runtime_fallback_error_shape::get_runtime_fallback_status_code;
pub use crate::runtime_fallback_retryable_patterns::RUNTIME_FALLBACK_RETRYABLE_ERROR_PATTERNS;
use crate::runtime_fallback_retryable_patterns::matches_retryable_pattern;

/// Runtime error categories callers branch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeFallbackErrorType {
    MissingApiKey,
    InvalidApiKey,
    ModelNotFound,
    QuotaExceeded,
    ContextOverflow,
    Abort,
}

impl RuntimeFallbackErrorType {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingApiKey => "missing_api_key",
            Self::InvalidApiKey => "invalid_api_key",
            Self::ModelNotFound => "model_not_found",
            Self::QuotaExceeded => "quota_exceeded",
            Self::ContextOverflow => "context_overflow",
            Self::Abort => "abort",
        }
    }
}

/// Reported when an `isRetryable: true` signal is ignored because its status code is unsafe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsafeRetryableSignalRejected<'a> {
    pub status_code: i64,
    pub retry_on_errors: &'a [i64],
}

#[derive(Default)]
pub struct RuntimeFallbackRetryOptions<'a> {
    pub on_unsafe_retryable_signal_rejected: Option<&'a dyn Fn(UnsafeRetryableSignalRejected<'_>)>,
}

#[expect(clippy::expect_used, reason = "static regex literals are valid")]
fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid regex")
}

fn is_status_code_retry_safe(code: i64, retry_on_errors: &[i64]) -> bool {
    retry_on_errors.contains(&code)
        || (500..600).contains(&code)
        || code == 408
        || code == 425
        || code == 429
}

fn is_localized_quota_exhaustion_message(message: &str) -> bool {
    (message.contains("预扣费额度失败") && message.contains("用户剩余额度"))
        || (message.contains("用户剩余额度") && message.contains("需要预扣费额度"))
}

fn get_detail_error_type(error: Option<&Value>) -> Option<String> {
    let error = error.filter(|error| error.is_object())?;
    let data = error.get("data");
    let detail = error
        .get("detail")
        .filter(|value| !value.is_null())
        .or_else(|| data.and_then(|data| data.get("detail")).filter(|value| !value.is_null()))
        .unwrap_or(error);
    let detail_error = detail
        .get("error")
        .filter(|value| !value.is_null())
        .unwrap_or(detail);
    let error_type = detail_error
        .get("type")
        .filter(|value| !value.is_null())
        .or_else(|| detail.get("type"))
        .and_then(Value::as_str);
    error_type.map(str::to_lowercase)
}

static NON_TERMINAL_QUOTA_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)\bnon[-\s]+terminal\s+quota\b"));
static NON_TERMINAL_BILLING_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)\bnon[-\s]+terminal\s+billing\s+limit\b"));
static TERMINAL_QUOTA_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)\bterminal\s+quota\b"));
static TERMINAL_BILLING_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)\bterminal\s+billing\s+limit\b"));
static HARD_BILLING_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)\bhard\s+billing\s+limit\b"));

#[must_use]
fn is_terminal_quota_message(message: &str) -> bool {
    if NON_TERMINAL_QUOTA_PATTERN.is_match(message)
        || NON_TERMINAL_BILLING_PATTERN.is_match(message)
    {
        return false;
    }
    TERMINAL_QUOTA_PATTERN.is_match(message)
        || TERMINAL_BILLING_PATTERN.is_match(message)
        || HARD_BILLING_PATTERN.is_match(message)
}

/// A quota the provider marks as permanent: no reset, so no fallback retry either.
#[must_use]
pub fn is_terminal_quota_error(error: Option<&Value>) -> bool {
    get_detail_error_type(error).as_deref() == Some("terminal_quota_exhausted")
        || is_terminal_quota_message(&get_runtime_fallback_error_message(error))
}

static QUOTA_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)quota.?exceeded",
        r"(?i)exceeded.*quota",
        r"(?i)quota\b.*\breset",
        r"(?i)usage\s*quota",
        r"(?i)subscription.?(?:quota|limit)",
        r"(?i)insufficient.?(?:quota|balance|funds?|credits?)",
        r"(?i)credits?\s+exhausted",
        r"(?i)\b(?:session|weekly|monthly|daily|hourly|\d+[-\s]hour|plan|call)\s+limit\b",
        r"(?i)\bhit\s+your\b[^.]*\blimit\b",
        r"(?i)\bin\s+arrears\b",
        r"(?i)\brecharge\s+and\s+try\b",
        r"(?i)billing.?(?:hard.?)?limit",
        r"(?i)exhausted\s+your\s+capacity",
        r"(?i)resource.?exhausted",
        r"(?i)out\s+of\s+credits?",
        r"(?i)payment.?required",
        r"(?i)usage\s+limit",
        r"(?i)credit\s+balance.*too\s+low",
        r"(?i)limit\s+exhausted",
        r"使用上限",
        r"用量上限",
        r"达到.*限制",
        r"额度.*不足",
        r"余额.*不足",
        r"已耗尽",
    ]
    .iter()
    .map(|pattern| regex(pattern))
    .collect()
});

#[must_use]
pub fn classify_runtime_fallback_error(error: Option<&Value>) -> Option<RuntimeFallbackErrorType> {
    if is_terminal_quota_error(error) {
        return Some(RuntimeFallbackErrorType::Abort);
    }

    static API_KEY_MISSING: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)api.?key.?is.?missing"));
    static ENVIRONMENT_VARIABLE: LazyLock<Regex> =
        LazyLock::new(|| regex(r"(?i)environment variable"));
    static API_KEY: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)api.?key"));
    static MUST_BE_STRING: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)must be a string"));
    static MODEL_NOT_FOUND: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)model\s+not\s+found"));

    let message = get_runtime_fallback_error_message(error);
    let error_name = get_runtime_fallback_error_name(error)
        .map(|name| name.to_lowercase().replace(['_', '-'], ""));
    let name_contains = |needle: &str| {
        error_name
            .as_deref()
            .is_some_and(|name| name.contains(needle))
    };

    if name_contains("messageabortederror") || name_contains("aborterror") {
        return Some(RuntimeFallbackErrorType::Abort);
    }
    if error_name.as_deref() == Some("contextoverflowerror") {
        return Some(RuntimeFallbackErrorType::ContextOverflow);
    }
    if name_contains("ailoadapikeyerror")
        || name_contains("loadapi")
        || (API_KEY_MISSING.is_match(&message) && ENVIRONMENT_VARIABLE.is_match(&message))
    {
        return Some(RuntimeFallbackErrorType::MissingApiKey);
    }
    if API_KEY.is_match(&message) && MUST_BE_STRING.is_match(&message) {
        return Some(RuntimeFallbackErrorType::InvalidApiKey);
    }
    if name_contains("providermodelnotfounderror")
        || name_contains("modelnotfounderror")
        || (name_contains("unknownerror") && MODEL_NOT_FOUND.is_match(&message))
    {
        return Some(RuntimeFallbackErrorType::ModelNotFound);
    }
    if name_contains("quotaexceeded")
        || name_contains("insufficientquota")
        || name_contains("billingerror")
        || name_contains("resourceexhausted")
        || name_contains("insufficientcredits")
        || name_contains("usagelimit")
        || QUOTA_PATTERNS
            .iter()
            .any(|pattern| pattern.is_match(&message))
        || is_localized_quota_exhaustion_message(&message)
    {
        return Some(RuntimeFallbackErrorType::QuotaExceeded);
    }
    None
}

#[must_use]
pub fn is_runtime_fallback_retryable_error(
    error: Option<&Value>,
    retry_on_errors: &[i64],
    options: &RuntimeFallbackRetryOptions<'_>,
) -> bool {
    let status_code = get_runtime_fallback_status_code(error, Some(retry_on_errors));
    let message = get_runtime_fallback_error_message(error);

    match classify_runtime_fallback_error(error) {
        // OpenCode starts native compaction for context overflow; fallback would abort it.
        Some(RuntimeFallbackErrorType::Abort | RuntimeFallbackErrorType::ContextOverflow) => {
            return false;
        }
        Some(
            RuntimeFallbackErrorType::MissingApiKey
            | RuntimeFallbackErrorType::ModelNotFound
            | RuntimeFallbackErrorType::QuotaExceeded,
        ) => return true,
        Some(RuntimeFallbackErrorType::InvalidApiKey) | None => {}
    }

    if let Some(code) = status_code.filter(|code| *code != 0)
        && retry_on_errors.contains(&code)
    {
        return true;
    }

    if get_runtime_fallback_retryable_signal(error) == Some(true) {
        match status_code {
            None => return true,
            Some(code) if is_status_code_retry_safe(code, retry_on_errors) => return true,
            Some(code) => {
                if let Some(callback) = options.on_unsafe_retryable_signal_rejected {
                    callback(UnsafeRetryableSignalRejected {
                        status_code: code,
                        retry_on_errors,
                    });
                }
            }
        }
    }

    matches_retryable_pattern(&message)
}
