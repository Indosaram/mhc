//! The late `<memory_notice>` message (port of `prompt-notice.ts`, extracted from `prompt.ts`).

use crate::projection_expiry::render_expired_line;
use crate::projection_pin::{ProjectionTurn, render_projected_changes_line};
use memory_core::agreements::AgreementItem;
use memory_core::compile::compile::MemoryExpiry;

pub const MEMORY_NUDGE_METADATA_TOKEN: &str = "user turns since your last memory save";
pub const MEMORY_SOUL_METADATA_TOKEN: &str = "Soul updated by";
pub const MEMORY_AGREEMENT_METADATA_TOKEN: &str = "Agreement NEEDS-HUMAN";

/// Upstream `MemoryNoticeInputs`.
pub struct MemoryNoticeInputs<'a> {
    pub previous_message_count: usize,
    pub nudge_turns: Option<usize>,
    pub soul_sha: Option<&'a str>,
    pub turn: &'a ProjectionTurn,
    pub expired: &'a [MemoryExpiry],
    pub flagged_agreements: &'a [AgreementItem],
}

/// Session-volatile lines only. Every line is a fact about THIS session's state.
pub fn render_memory_notice(inputs: &MemoryNoticeInputs<'_>) -> String {
    let previous_message_count = inputs.previous_message_count;
    let mut lines = vec![
        "<memory_notice>".to_owned(),
        format!(
            "- {previous_message_count} previous messages between you and the user are stored in recall memory"
        ),
    ];
    if let Some(turns) = inputs.nudge_turns {
        lines.push(format!("- {turns} {MEMORY_NUDGE_METADATA_TOKEN}. Save durable facts now, or decide nothing qualifies."));
    }
    if let Some(sha) = inputs.soul_sha {
        lines.push(format!(
            "- {MEMORY_SOUL_METADATA_TOKEN} reflection {} since your last run",
            sha.chars().take(7).collect::<String>()
        ));
    }
    if let Some(changes) = &inputs.turn.changes {
        lines.push(render_projected_changes_line(
            changes,
            inputs.turn.revision.as_deref(),
            &inputs.turn.sources,
        ));
    }
    if !inputs.expired.is_empty() {
        lines.push(render_expired_line(inputs.expired));
    }
    for item in inputs.flagged_agreements {
        lines.push(format!("- {MEMORY_AGREEMENT_METADATA_TOKEN} (broken x{}): \"{}\" Do not break it again this session; if the task needs it, stop and ask your person.",item.n,item.text));
    }
    lines.push("</memory_notice>".to_owned());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_expired_line_sits_between_changes_and_agreements() {
        let turn = ProjectionTurn::default();
        let expired = [MemoryExpiry {
            path: "system/promo.md".into(),
            expires_at_ms: 1,
        }];
        let notice = render_memory_notice(&MemoryNoticeInputs {
            previous_message_count: 2,
            nudge_turns: None,
            soul_sha: None,
            turn: &turn,
            expired: &expired,
            flagged_agreements: &[],
        });
        assert_eq!(
            notice,
            "<memory_notice>\n- 2 previous messages between you and the user are stored in recall memory\n- Expired since this prompt was pinned (no longer true): system/promo.md.\n</memory_notice>"
        );
    }
}
