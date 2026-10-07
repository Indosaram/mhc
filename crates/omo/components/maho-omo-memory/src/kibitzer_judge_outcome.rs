//! Judge turn classification (latest `kibitzer/judge-outcome.ts`).
//!
//! The upstream pattern is `/\b503\b|auth[_ -]?unavailable|overloaded/iu`: `503` is WORD-BOUNDED
//! (so `1503` never matches) and `auth[_ -]?unavailable` allows AT MOST ONE separator (so
//! `auth---unavailable` never matches). The reason cap counts UTF-16 units, like JS `.length`.

use memory_core::recall::RecallNudge;
use memory_core::sync::redact::contains_secret_like_material;

/// The gate reason cap, mirroring `kibitzer/notice.ts` `GATE_REASON_MAX_CHARS`.
pub const GATE_REASON_MAX_CHARS: usize = 160;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JudgeTurnClassification {
    Completed,
    Empty,
    Failed { cause: JudgeFailureCause, reason: Option<String> },
    Dropped { cause: JudgeDropCause },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JudgeFailureCause { ChildFailed, ChildFailedUpstream }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JudgeDropCause { Cancelled }

/// senpi's empty-assistant recovery settles the second silent stop as this message; for THIS child
/// it describes a completed turn, not a failure (issue #7963).
const EMPTY_RESPONSE_TWICE: &str = "Model returned an empty response twice";

/// `classifyJudgeTurn(outcome, accepted)`. `settle` is the child's structured `JudgeSettle`.
pub fn classify_judge_turn(settle: &crate::kibitzer_child::JudgeSettle, accepted: &[RecallNudge]) -> JudgeTurnClassification {
    if settle.completed {
        return JudgeTurnClassification::Completed;
    }
    if settle.cancelled {
        return JudgeTurnClassification::Dropped { cause: JudgeDropCause::Cancelled };
    }
    let reason = normalize_gate_reason(settle.failure_message.as_deref());
    if reason.as_deref() == Some(EMPTY_RESPONSE_TWICE) {
        return if accepted.is_empty() { JudgeTurnClassification::Empty } else { JudgeTurnClassification::Completed };
    }
    let cause = match &reason {
        Some(reason) if is_upstream_failure(reason) => JudgeFailureCause::ChildFailedUpstream,
        _ => JudgeFailureCause::ChildFailed,
    };
    JudgeTurnClassification::Failed { cause, reason }
}

fn is_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

/// `\b503\b`: a `503` run whose neighbouring chars are not word chars.
fn has_bounded_503(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = 0usize;
    while let Some(relative) = text[from..].find("503") {
        let at = from + relative;
        let after = at + 3;
        let before_ok = at == 0 || !text[..at].chars().next_back().is_some_and(is_word_char);
        let after_ok = after >= bytes.len() || !text[after..].chars().next().is_some_and(is_word_char);
        if before_ok && after_ok {
            return true;
        }
        from = at + 1;
    }
    false
}

/// `auth[_ -]?unavailable`: `auth`, at most ONE `_`/space/`-`, then `unavailable`.
fn has_auth_unavailable(text: &str) -> bool {
    let mut from = 0usize;
    while let Some(relative) = text[from..].find("auth") {
        let at = from + relative;
        let rest = &text[at + 4..];
        let matched = rest.strip_prefix("unavailable").is_some()
            || ["_", " ", "-"].iter().any(|separator| rest.strip_prefix(separator).and_then(|r| r.strip_prefix("unavailable")).is_some());
        if matched {
            return true;
        }
        from = at + 1;
    }
    false
}

/// `\b503\b|auth[_ -]?unavailable|overloaded` (unicode, case-insensitive).
fn is_upstream_failure(reason: &str) -> bool {
    let lower = reason.to_lowercase();
    has_bounded_503(&lower) || has_auth_unavailable(&lower) || lower.contains("overloaded")
}

/// JS `String.length` in UTF-16 units.
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// JS `String.slice(0, n)` then drop a dangling high surrogate.
fn utf16_head(text: &str, n: usize) -> String {
    let mut units: Vec<u16> = text.encode_utf16().take(n).collect();
    if let Some(&last) = units.last()
        && (0xd800..=0xdbff).contains(&last)
    {
        units.pop();
    }
    String::from_utf16_lossy(&units)
}

/// `normalizeGateReason`: strip control chars, collapse whitespace, redact secrets, cap the length.
pub fn normalize_gate_reason(message: Option<&str>) -> Option<String> {
    let message = message?;
    let stripped: String = message.chars().filter(|ch| !matches!(ch, '\u{0000}'..='\u{001f}' | '\u{007f}'..='\u{009f}')).collect();
    let collapsed = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    if contains_secret_like_material(&collapsed) {
        return Some("redacted".to_string());
    }
    if utf16_len(&collapsed) > GATE_REASON_MAX_CHARS {
        Some(format!("{}\u{2026}", utf16_head(&collapsed, GATE_REASON_MAX_CHARS - 1)))
    } else {
        Some(collapsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_a_bounded_503_when_classified_then_it_is_upstream() {
        assert!(is_upstream_failure("HTTP 503 Service Unavailable"));
        assert!(!is_upstream_failure("1503 records"));
        assert!(!is_upstream_failure("error 5034"));
    }

    #[test]
    fn given_an_auth_failure_with_one_separator_when_classified_then_it_is_upstream() {
        assert!(is_upstream_failure("auth_unavailable"));
        assert!(is_upstream_failure("auth unavailable"));
        assert!(is_upstream_failure("auth-unavailable"));
        assert!(!is_upstream_failure("auth---unavailable"));
        assert!(!is_upstream_failure("authorization unavailable"));
    }

    #[test]
    fn given_overloaded_when_classified_then_it_is_upstream() {
        assert!(is_upstream_failure("The model is overloaded right now"));
    }

    #[test]
    fn given_a_long_reason_when_normalized_then_it_is_capped_with_the_ellipsis() {
        let long = "x".repeat(GATE_REASON_MAX_CHARS + 40);
        let normalized = normalize_gate_reason(Some(&long)).unwrap();
        assert_eq!(utf16_len(&normalized), GATE_REASON_MAX_CHARS);
        assert!(normalized.ends_with('\u{2026}'));
    }
}

