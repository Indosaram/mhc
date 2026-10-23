use std::sync::LazyLock;

use regex::RegexSet;

/// Pattern table mirrored from senpi packages/ai/src/utils/overflow.ts.
// PERF-DIVERGENCE(senpi packages/ai/src/utils/overflow.ts): upstream tests each overflow pattern separately per message; this port compiles the same patterns into one RegexSet for a single-pass any-match. The boolean result is identical.
static OVERFLOW_PATTERNS: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r"(?i)prompt is too long",
        r"(?i)request_too_large",
        r"(?i)input is too long for requested model",
        r"(?i)exceeds the context window",
        r"(?i)exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))",
        r"(?i)input token count.*exceeds the maximum",
        r"(?i)maximum prompt length is \d+",
        r"(?i)reduce the length of the messages",
        r"(?i)maximum context length is \d+ tokens",
        r"(?i)exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?",
        r"(?i)input \(\d+ tokens\) is longer than the model'?s context length \d+ tokens",
        r"(?i)exceeds the limit of \d+",
        r"(?i)exceeds the available context size",
        r"(?i)greater than the context length",
        r"(?i)context window exceeds limit",
        r"(?i)exceeded model token limit",
        r"(?i)too large for model with \d+ maximum context length",
        r"(?i)prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?",
        r"(?i)prompt too long; exceeded (?:max )?context length",
        r"(?i)range of input length should be",
        r"(?i)model_context_window_exceeded",
        r"(?i)context[_ ]length[_ ]exceeded",
        r"(?i)too many tokens",
        r"(?i)token limit exceeded",
        r"(?i)^4(?:00|13)\s*(?:status code)?\s*\(no body\)",
        r"(?i)(?:request[ _])?(?:body|entity|payload)[_ ]too[_ ]large",
        r"(?i)Request payload is \d+ (?:bytes, over the \d+ byte|tokens, over the \d+ token) limit Kiro accepts\.",
        r"(?i)Model context limit reached\. Conversation size exceeds model capacity\.",
    ])
    .expect("valid regex set")
});

// PERF-DIVERGENCE(senpi packages/ai/src/utils/overflow.ts): same single-pass RegexSet replacement for the non-overflow guard list. The boolean result is identical.
static NON_OVERFLOW_PATTERNS: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r"(?i)^(Throttling error|Service unavailable):",
        r"(?i)rate limit",
        r"(?i)too many requests",
    ])
    .expect("valid regex set")
});

#[must_use]
pub fn is_context_overflow_message(message: &str) -> bool {
    !NON_OVERFLOW_PATTERNS.is_match(message) && OVERFLOW_PATTERNS.is_match(message)
}
