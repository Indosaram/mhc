//! Port of `claim-receipt/runtime.ts`: evaluate one finished answer.
//!
//! The upstream sidecar packaging comments are JS-only; the orchestration itself is native.

use serde_json::Value;

use crate::classify::classify_claims;
use crate::evidence::{build_timeline, content_text};
use crate::r#match::{is_flagged, match_claims, ClaimVerdict};

/// Every verdict for the answer plus the unsupported ones.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnswerVerdicts {
    pub verdicts: Vec<ClaimVerdict>,
    pub flagged: Vec<ClaimVerdict>,
}

/// Classifies the answer's text, builds the branch timeline and matches the two. A claim-free
/// answer has nothing to evaluate.
#[must_use]
pub fn evaluate_answer(
    branch: &[Value],
    answer_at: f64,
    content: &Value,
) -> Option<AnswerVerdicts> {
    let claims = classify_claims(&content_text(content));
    if claims.is_empty() {
        return None;
    }
    let verdicts = match_claims(&build_timeline(branch), &claims, answer_at);
    let flagged = verdicts
        .iter()
        .filter(|verdict| is_flagged(verdict))
        .cloned()
        .collect();
    Some(AnswerVerdicts { verdicts, flagged })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::r#match::ClaimStatus;

    fn entries() -> Vec<Value> {
        serde_json::json!([
            {
                "type": "message",
                "id": "u",
                "message": { "role": "user", "content": [{ "type": "text", "text": "fix it" }] }
            },
            {
                "type": "message",
                "id": "a",
                "message": { "role": "assistant", "content": [{ "type": "text", "text": "All tests pass." }] }
            }
        ])
        .as_array()
        .cloned()
        .unwrap_or_default()
    }

    #[test]
    fn an_unsupported_claim_is_flagged_and_a_claim_free_answer_is_not_evaluated() {
        let branch = entries();
        let content = serde_json::json!([{ "type": "text", "text": "All tests pass." }]);
        let evaluated = evaluate_answer(&branch, 1.0, &content).expect("evaluated");
        assert_eq!(evaluated.verdicts.len(), 1);
        assert_eq!(evaluated.flagged.len(), 1);
        assert_eq!(evaluated.verdicts[0].status, ClaimStatus::Unsupported);

        let prose = serde_json::json!([{ "type": "text", "text": "Here is the summary you asked for." }]);
        assert!(evaluate_answer(&branch, 1.0, &prose).is_none());
    }
}
