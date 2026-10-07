//! Visible trace for an injected recall hint (latest `recall-notice.ts`).
//!
//! The hint itself rides the model-facing hidden custom message; this entry channel is the
//! user-facing half: one compact house-notice line naming the surfaced paths.

use serde::{Deserialize, Serialize};

use crate::worker::completion_renderers::ResolveEntryTheme;
use crate::worker::entry_renderers::{NoticeComponent, NoticeSpec, join_fields};

/// Custom type of an injected recall block (mirrors `recall_session_read::RECALL_CUSTOM_TYPE`).
pub const RECALL_ENTRY_TYPE: &str = "omo-kibitzer:recall";

/// The user-facing record for one recall injection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryRecallRecord {
    pub paths: Vec<String>,
}

/// `renderRecallEntry`: `None` when the record has no drawable path.
pub fn recall_notice_spec(record: &MemoryRecallRecord) -> Option<NoticeSpec> {
    let paths: Vec<String> = record
        .paths
        .iter()
        .map(|path| senpi_task::renderer_text::normalize_renderer_text(path))
        .filter(|path| !path.is_empty())
        .collect();
    if paths.is_empty() {
        return None;
    }
    Some(NoticeSpec {
        glyph: "\u{00b7}".into(),
        title: join_fields(&[Some("Memory recalled"), Some(&paths.join(", "))]),
        tone: "muted".into(),
        why: "A stored memory matched the previous turn; it is a hint, not current state.".into(),
        extra: vec![],
        detail: None,
    })
}

/// Registers the recall entry renderer.
pub fn register_recall_notice_renderer(api: &mut maho_ext_api::ExtensionApi, theme: ResolveEntryTheme) {
    api.register_entry_renderer(
        RECALL_ENTRY_TYPE,
        std::sync::Arc::new(move |entry, options, native_theme| {
            let data = entry.data.get("data")?.clone();
            let record: MemoryRecallRecord = serde_json::from_value(data).ok()?;
            let spec = recall_notice_spec(&record)?;
            Some(Box::new(NoticeComponent {
                spec,
                expanded: options.expanded,
                theme: theme(native_theme),
            }) as Box<dyn maho_ext_api::Component>)
        }),
        Default::default(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::entry_renderers::EntryRenderTheme;

    struct Theme;
    impl EntryRenderTheme for Theme {
        fn fg(&self, tone: &str, text: &str) -> String { format!("<{tone}>{text}</{tone}>") }
        fn italic(&self, text: &str) -> String { format!("<italic>{text}</italic>") }
    }

    #[test]
    fn given_paths_when_rendered_then_the_title_names_them() {
        let spec = recall_notice_spec(&MemoryRecallRecord { paths: vec!["notes/a.md".into(), "notes/b.md".into()] }).unwrap();
        assert_eq!(spec.glyph, "\u{00b7}");
        assert_eq!(spec.tone, "muted");
        assert!(spec.title.contains("notes/a.md, notes/b.md"));
    }

    #[test]
    fn given_blank_paths_when_rendered_then_nothing_is_drawn() {
        assert!(recall_notice_spec(&MemoryRecallRecord { paths: vec![] }).is_none());
        assert!(recall_notice_spec(&MemoryRecallRecord { paths: vec!["".into(), "  ".into()] }).is_none());
    }

    #[test]
    fn given_a_registered_renderer_when_the_entry_renders_then_the_notice_component_is_returned() {
        let mut api = maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("memory", Default::default(), Default::default()), Default::default(), Default::default(), Default::default());
        register_recall_notice_renderer(&mut api, std::sync::Arc::new(|_| std::sync::Arc::new(Theme)));
        let entry = maho_ext_api::SessionEntry { id: "entry".into(), parent_id: None, timestamp: "now".into(), kind: "custom".into(), data: serde_json::json!({ "data": { "paths": ["notes/a.md"] } }) };
        let renderer = &api.registered.entry_renderers[RECALL_ENTRY_TYPE];
        let mut component = renderer(&entry, &maho_ext_api::EntryRenderOptions { expanded: false }, &Default::default()).unwrap();
        let lines = component.render(80);
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().any(|line| line.contains("notes/a.md")));
    }
}
