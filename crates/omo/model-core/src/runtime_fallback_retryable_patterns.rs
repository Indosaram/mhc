use std::sync::LazyLock;

use regex::{Regex, RegexSet};

/// Message patterns that mark a runtime error as worth a fallback retry.
#[expect(clippy::expect_used, reason = "static regex literals are valid")]
pub static RUNTIME_FALLBACK_RETRYABLE_ERROR_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    RETRYABLE_PATTERN_SOURCES
        .iter()
        .map(|pattern| Regex::new(pattern).expect("valid regex"))
        .collect()
});

const RETRYABLE_PATTERN_SOURCES: [&str; 27] = [
    r"(?i)rate.?limit",
    r"(?i)too.?many.?requests",
    r"(?i)quota\s+will\s+reset\s+after",
    r"(?i)quota.?exceeded",
    r"(?i)exceeded.*quota",
    r"(?i)usage\s*quota",
    r"(?i)free.?usage",
    r"(?i)usage.?exceeded",
    r"(?i)exhausted\s+your\s+capacity",
    r"(?i)limit\s+exhausted",
    r"(?i)all\s+credentials\s+for\s+model",
    r"(?i)cool(?:ing)?\s+down",
    r"(?i)model.{0,20}?not.{0,10}?supported",
    r"(?i)model_not_supported",
    r"(?i)service.?unavailable",
    r"(?i)overloaded",
    r"(?i)temporarily.?unavailable",
    r"(?i)try.?again",
    r"(?:^|\s)429(?:\s|$)",
    r"(?:^|\s)503(?:\s|$)",
    r"(?:^|\s)529(?:\s|$)",
    r"使用上限",
    r"频率限制",
    r"请求过于频繁",
    r"暂时不可用",
    r"服务不可用",
    r"请稍后重试",
];

// PERF-DIVERGENCE(omo runtime-fallback): upstream scans each retryable pattern separately per message; this port compiles the same patterns into one RegexSet for a single-pass any-match. The boolean result is identical.
#[expect(clippy::expect_used, reason = "static regex literals are valid")]
static RETRYABLE_PATTERN_SET: LazyLock<RegexSet> =
    LazyLock::new(|| RegexSet::new(RETRYABLE_PATTERN_SOURCES).expect("valid regex set"));

pub(crate) fn matches_retryable_pattern(message: &str) -> bool {
    RETRYABLE_PATTERN_SET.is_match(message)
}
