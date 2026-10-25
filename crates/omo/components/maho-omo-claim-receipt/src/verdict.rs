//! Port of `claim-receipt/verdict.ts`: the persisted verdict record and the advisory blocks.

use serde_json::{json, Value};

use crate::r#match::{ClaimStatus, ClaimVerdict, UnsupportedReason};

pub const VERDICT_ENTRY_TYPE: &str = "omo-claim-receipt:verdict";
pub const NUDGE_MESSAGE_TYPE: &str = "omo-claim-receipt:nudge";
pub const GATE_FLAGS_TAG: &str = "claim-receipt-flags";

/// The version-1 record persisted after each finished answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerdictRecord {
    pub version: u8,
    pub answer_entry_id: String,
    pub epoch: u64,
    pub flagged: bool,
    pub nudged: bool,
    pub verdicts: Vec<ClaimVerdict>,
}

impl VerdictRecord {
    pub const VERSION: u8 = 1;

    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({
            "version": self.version,
            "answerEntryId": self.answer_entry_id,
            "epoch": self.epoch,
            "flagged": self.flagged,
            "nudged": self.nudged,
            "verdicts": self.verdicts.iter().map(verdict_value).collect::<Vec<_>>(),
        })
    }
}

/// One verdict as the record, the nudge details and the gate block carry it.
#[must_use]
pub fn verdict_value(verdict: &ClaimVerdict) -> Value {
    let claim = &verdict.claim;
    json!({
        "claim": {
            "kind": claim.kind.as_str(),
            "strength": claim.strength.as_str(),
            "admitted": claim.admitted,
            "reported": claim.reported,
            "sentence": claim.sentence,
            "rule": claim.rule,
        },
        "status": verdict.status.as_str(),
        "reason": verdict.reason.map(UnsupportedReason::as_str),
        "receipt": verdict.receipt.as_ref().map(|receipt| json!({
            "entryId": receipt.entry_id,
            "kind": receipt.kind.as_str(),
            "outcome": receipt.outcome.as_str(),
        })),
        "invalidatedBy": verdict.invalidated_by.as_ref().map(|invalidated| json!({
            "entryId": invalidated.entry_id,
            "paths": invalidated.paths,
        })),
    })
}

fn reason_text(reason: UnsupportedReason) -> &'static str {
    match reason {
        UnsupportedReason::NoCheck => "no check ran before this claim",
        UnsupportedReason::LastCheckFailed => "the latest matching check before this claim failed",
        UnsupportedReason::StaleAfterEdit => "code was edited after the last passing check",
    }
}

fn quote(sentence: &str) -> String {
    let shortened: String = if sentence.chars().count() > 200 {
        let mut truncated: String = sentence.chars().take(197).collect();
        truncated.push_str("...");
        truncated
    } else {
        sentence.to_owned()
    };
    serde_json::to_string(&shortened).unwrap_or_else(|_| format!("\"{shortened}\""))
}

fn describe(verdict: &ClaimVerdict) -> String {
    let reason = verdict
        .reason
        .map_or_else(|| ClaimStatus::Unsupported.as_str(), reason_text);
    let check = verdict.receipt.as_ref().map_or_else(String::new, |receipt| {
        format!(
            " (check entry {}, {} {})",
            receipt.entry_id,
            receipt.kind.as_str(),
            receipt.outcome.as_str()
        )
    });
    let edit = verdict
        .invalidated_by
        .as_ref()
        .map_or_else(String::new, |invalidated| {
            let paths = if invalidated.paths.is_empty() {
                String::new()
            } else {
                format!(": {}", invalidated.paths.join(", "))
            };
            format!(" (edit entry {}{paths})", invalidated.entry_id)
        });
    format!(
        "- {}: {reason}{check}{edit}",
        quote(&verdict.claim.sentence)
    )
}

#[must_use]
pub fn nudge_text(flagged: &[ClaimVerdict]) -> String {
    let mut lines = vec![
        "<claim-receipt>".to_owned(),
        "Your last answer states results that no check result in this session supports:".to_owned(),
    ];
    lines.extend(flagged.iter().map(describe));
    lines.push("Run the check and report its actual result, or say plainly that it was not verified. This is advisory and is shown once per user message.".to_owned());
    lines.push("</claim-receipt>".to_owned());
    lines.join("\n")
}

#[must_use]
pub fn gate_flags_block(flagged: &[ClaimVerdict]) -> String {
    let mut lines = vec![
        format!("<{GATE_FLAGS_TAG}>"),
        "The parent session made these success claims without a supporting check result (claim-receipt). Treat each as unverified and reproduce it from the artifacts:".to_owned(),
    ];
    lines.extend(flagged.iter().map(describe));
    lines.push(format!("</{GATE_FLAGS_TAG}>"));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::classify_claims;
    use crate::evidence::{CheckKind, CheckOutcome};
    use crate::r#match::Receipt;

    fn unsupported(sentence: &str) -> ClaimVerdict {
        ClaimVerdict {
            claim: classify_claims(sentence)
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("one claim for {sentence:?}")),
            status: ClaimStatus::Unsupported,
            reason: Some(UnsupportedReason::NoCheck),
            receipt: None,
            invalidated_by: None,
        }
    }

    #[test]
    fn the_nudge_and_the_gate_block_quote_the_flagged_sentence() {
        let flagged = vec![unsupported("All tests pass.")];
        let nudge = nudge_text(&flagged);
        assert!(nudge.starts_with("<claim-receipt>\n"));
        assert!(nudge.contains("- \"All tests pass.\": no check ran before this claim"));
        assert!(nudge.ends_with("</claim-receipt>"));

        let gate = gate_flags_block(&flagged);
        assert!(gate.starts_with(&format!("<{GATE_FLAGS_TAG}>\n")));
        assert!(gate.ends_with(&format!("</{GATE_FLAGS_TAG}>")));
    }

    #[test]
    fn a_receipt_and_an_invalidating_edit_are_annotated() {
        let mut verdict = unsupported("All tests pass.");
        verdict.status = ClaimStatus::Supported;
        verdict.reason = None;
        verdict.receipt = Some(Receipt {
            entry_id: "t1".to_owned(),
            kind: CheckKind::Test,
            outcome: CheckOutcome::Passed,
        });
        assert_eq!(
            describe(&verdict),
            "- \"All tests pass.\": unsupported (check entry t1, test passed)"
        );
        verdict.status = ClaimStatus::Unsupported;
        verdict.reason = Some(UnsupportedReason::StaleAfterEdit);
        verdict.invalidated_by = Some(crate::r#match::InvalidatedBy {
            entry_id: "e1".to_owned(),
            paths: vec!["src/a.ts".to_owned()],
        });
        assert_eq!(
            describe(&verdict),
            "- \"All tests pass.\": code was edited after the last passing check (check entry t1, test passed) (edit entry e1: src/a.ts)"
        );
    }

    #[test]
    fn long_sentences_are_truncated_to_a_bounded_quoted_length() {
        let long = format!("{} All tests pass.", "x".repeat(300));
        let quoted = quote(&long);
        assert_eq!(quoted.chars().count(), 197 + 3 + 2);
        assert!(quoted.ends_with("...\""));
    }
}
