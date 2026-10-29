//! Needs-human surface for the person (pin `agreement-notice.ts`). Like `soul_notice.rs`, this is a
//! NON-MODEL-FACING session entry (`append_entry` + entry renderer); the agent learns about the same
//! flag through the `<memory_notice>` line.
use serde::{Deserialize, Serialize};

use crate::worker::entry_renderers::{
    NoticeComponent, NoticeExtraLine, NoticeSpec, join_fields, normalize_renderer_text,
};

pub const AGREEMENT_FLAGGED_ENTRY_TYPE: &str = "omo-memory:agreement-flagged";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgreementFlaggedRecord {
    pub id: String,
    pub text: String,
    pub n: u64,
    /// `"human"` or `"agent"`.
    pub by: String,
}

/// The notice spec for one flagged agreement.
pub fn agreement_flagged_spec(record: &AgreementFlaggedRecord) -> NoticeSpec {
    let id = normalize_renderer_text(&record.id);
    let author = if record.by == "human" {
        "written by you"
    } else {
        "learned by the agent"
    };
    NoticeSpec {
        glyph: "!".into(),
        title: join_fields(&[Some("Agreement"), Some("keeps being broken")]),
        tone: "warning".into(),
        why: format!(
            "\"{}\" was broken {} times.",
            normalize_renderer_text(&record.text),
            record.n
        ),
        extra: vec![NoticeExtraLine {
            text: format!("/agreements ack {id} resets it; /agreements drop {id} removes it."),
            tone: Some("dim".into()),
        }],
        detail: Some(join_fields(&[Some(&id), Some(author)])),
    }
}

/// Renderer body for [`AGREEMENT_FLAGGED_ENTRY_TYPE`] entries.
pub fn render_agreement_flagged_entry(
    record: &AgreementFlaggedRecord,
    expanded: bool,
    theme: std::sync::Arc<dyn crate::worker::entry_renderers::EntryRenderTheme>,
) -> NoticeComponent {
    NoticeComponent {
        spec: agreement_flagged_spec(record),
        expanded,
        theme,
    }
}

/// Registers the renderer next to the soul one (pin `memory-notice-wiring.ts`).
pub fn register_agreement_notice(
    api: &mut maho_ext_api::ExtensionApi,
    theme: crate::worker::completion_renderers::ResolveEntryTheme,
) {
    api.register_entry_renderer(
        AGREEMENT_FLAGGED_ENTRY_TYPE,
        std::sync::Arc::new(move |entry, options, native_theme| {
            let record: AgreementFlaggedRecord =
                serde_json::from_value(entry.data.get("data")?.clone()).ok()?;
            Some(Box::new(render_agreement_flagged_entry(
                &record,
                options.expanded,
                theme(native_theme),
            )))
        }),
        Default::default(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(by: &str) -> AgreementFlaggedRecord {
        AgreementFlaggedRecord {
            id: "0123456789".into(),
            text: "no force pushes".into(),
            n: 3,
            by: by.into(),
        }
    }

    #[test]
    fn spec_names_the_break_count_and_the_resolving_commands() {
        let spec = agreement_flagged_spec(&record("human"));
        assert_eq!(spec.glyph, "!");
        assert_eq!(spec.tone, "warning");
        assert_eq!(spec.title, "Agreement · keeps being broken");
        assert_eq!(spec.why, "\"no force pushes\" was broken 3 times.");
        assert_eq!(
            spec.extra[0].text,
            "/agreements ack 0123456789 resets it; /agreements drop 0123456789 removes it."
        );
        assert_eq!(spec.extra[0].tone.as_deref(), Some("dim"));
        assert_eq!(spec.detail.as_deref(), Some("0123456789 · written by you"));
    }

    #[test]
    fn agent_authored_detail_reads_learned() {
        let spec = agreement_flagged_spec(&record("agent"));
        assert_eq!(
            spec.detail.as_deref(),
            Some("0123456789 · learned by the agent")
        );
    }
}
