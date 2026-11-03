use maho_core::compaction::compaction::estimate_tokens;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StructuralYield {
    pub saved_tokens: f64,
    pub savings_ratio: f64,
    pub tokens_before: f64,
}
fn approx_text_tokens(text: &str) -> u64 {
    estimate_tokens(&json!({"role":"assistant","content":[{"type":"text","text":text}]}))
}
pub fn compute_structural_yield(
    previous_summary: &str,
    messages_to_summarize: &[Value],
    turn_prefix_messages: &[Value],
    summary: &str,
    tokens_before: f64,
) -> StructuralYield {
    let replaced = approx_text_tokens(previous_summary)
        + messages_to_summarize
            .iter()
            .map(estimate_tokens)
            .sum::<u64>()
        + turn_prefix_messages
            .iter()
            .map(estimate_tokens)
            .sum::<u64>();
    let saved_tokens = replaced.saturating_sub(approx_text_tokens(summary)) as f64;
    StructuralYield {
        saved_tokens,
        savings_ratio: if tokens_before > 0.0 {
            saved_tokens / tokens_before
        } else {
            0.0
        },
        tokens_before,
    }
}
pub fn is_ineffective_compaction(value: StructuralYield) -> bool {
    value.tokens_before <= 0.0 || value.saved_tokens < 1024.0 || value.savings_ratio < 0.1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_tokens_count_utf16_units() {
        // senpi `text.length`: an astral character is two units.
        assert_eq!(approx_text_tokens(&"\u{1f600}".repeat(4)), 2);
        assert_eq!(approx_text_tokens("abcde"), 2);
    }
}
