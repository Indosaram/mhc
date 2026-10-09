use std::sync::LazyLock;

use regex::Regex;

/// Pattern table mirrored from senpi packages/ai/src/utils/overflow.ts.
static OVERFLOW_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"(?i)prompt is too long").expect("valid regex"),
        Regex::new(r"(?i)request_too_large").expect("valid regex"),
        Regex::new(r"(?i)input is too long for requested model").expect("valid regex"),
        Regex::new(r"(?i)exceeds the context window").expect("valid regex"),
        Regex::new(r"(?i)exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))").expect("valid regex"),
        Regex::new(r"(?i)input token count.*exceeds the maximum").expect("valid regex"),
        Regex::new(r"(?i)maximum prompt length is \d+").expect("valid regex"),
        Regex::new(r"(?i)reduce the length of the messages").expect("valid regex"),
        Regex::new(r"(?i)maximum context length is \d+ tokens").expect("valid regex"),
        Regex::new(r"(?i)exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?").expect("valid regex"),
        Regex::new(r"(?i)input \(\d+ tokens\) is longer than the model'?s context length \d+ tokens").expect("valid regex"),
        Regex::new(r"(?i)exceeds the limit of \d+").expect("valid regex"),
        Regex::new(r"(?i)exceeds the available context size").expect("valid regex"),
        Regex::new(r"(?i)greater than the context length").expect("valid regex"),
        Regex::new(r"(?i)context window exceeds limit").expect("valid regex"),
        Regex::new(r"(?i)exceeded model token limit").expect("valid regex"),
        Regex::new(r"(?i)too large for model with \d+ maximum context length").expect("valid regex"),
        Regex::new(r"(?i)prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?").expect("valid regex"),
        Regex::new(r"(?i)prompt too long; exceeded (?:max )?context length").expect("valid regex"),
        Regex::new(r"(?i)range of input length should be").expect("valid regex"),
        Regex::new(r"(?i)model_context_window_exceeded").expect("valid regex"),
        Regex::new(r"(?i)context[_ ]length[_ ]exceeded").expect("valid regex"),
        Regex::new(r"(?i)too many tokens").expect("valid regex"),
        Regex::new(r"(?i)token limit exceeded").expect("valid regex"),
        Regex::new(r"(?i)^4(?:00|13)\s*(?:status code)?\s*\(no body\)").expect("valid regex"),
        Regex::new(r"(?i)(?:request[ _])?(?:body|entity|payload)[_ ]too[_ ]large").expect("valid regex"),
        Regex::new(r"(?i)Request payload is \d+ (?:bytes, over the \d+ byte|tokens, over the \d+ token) limit Kiro accepts\.").expect("valid regex"),
        Regex::new(r"(?i)Model context limit reached\. Conversation size exceeds model capacity\.").expect("valid regex"),
    ]
});

static NON_OVERFLOW_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"(?i)^(Throttling error|Service unavailable):").expect("valid regex"),
        Regex::new(r"(?i)rate limit").expect("valid regex"),
        Regex::new(r"(?i)too many requests").expect("valid regex"),
    ]
});

#[must_use]
pub fn is_context_overflow_message(message: &str) -> bool {
    !NON_OVERFLOW_PATTERNS
        .iter()
        .any(|pattern| pattern.is_match(message))
        && OVERFLOW_PATTERNS
            .iter()
            .any(|pattern| pattern.is_match(message))
}
