use maho_agent::types::{AgentEvent, AgentMessage};
use maho_core::agent_session::AgentSession;
use maho_ext_api::{
    AgentSessionEvent, EntryRendererOptions, Extension, ExtensionApi, SessionEntry,
};
use maho_interactive::interactive_mode::InteractiveMode;
use maho_tui::{components::text::Text, tui::Component};
use serde_json::json;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Entries(Arc<Mutex<Vec<(u16, bool)>>>);

impl Extension for Entries {
    fn register(&self, api: &mut ExtensionApi) {
        for kind in ["goal", "note"] {
            let calls = Arc::clone(&self.0);
            api.register_entry_renderer(
                kind,
                Arc::new(move |entry, options, _| {
                    let n = serde_json::from_value::<u16>(entry.data["data"]["n"].clone())
                        .expect("numeric card data");
                    calls
                        .lock()
                        .expect("entries lock")
                        .push((n, options.expanded));
                    Some(Box::new(Text::with_padding(format!("[card:{n}]"), 0, 0)))
                }),
                EntryRendererOptions {
                    replaces: Some(Arc::new(|a, b| {
                        a.data["data"]["goal"] == b.data["data"]["goal"]
                    })),
                },
            );
        }
    }
}

struct Fixture {
    mode: InteractiveMode,
    session: Arc<AgentSession>,
    entries: Entries,
    _directory: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let (mode, directory, session) =
            crate::runtime35_native::native_mode_with_session_and_provider_retaining_session(
                None,
                |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
                "faux",
                "faux-1",
                false,
            );
        let entries = Entries::default();
        session
            .set_extension_runner(maho_ext_host::ExtensionRunner::from_static(
                vec![Box::new(entries.clone())],
                session.extension_context(mode.extension_ui.clone()),
            ))
            .await;
        Self {
            mode,
            session,
            entries,
            _directory: directory,
        }
    }

    fn append(&mut self, kind: &str, goal: &str, n: u16) {
        let data = self.session.with_session_manager_mut(|manager| {
            manager.append_custom(kind, Some(json!({"goal": goal, "n": n})))
        });
        let entry = SessionEntry {
            id: data["id"].as_str().expect("appended id").into(),
            parent_id: data["parentId"].as_str().map(str::to_owned),
            timestamp: data["timestamp"]
                .as_str()
                .expect("appended timestamp")
                .into(),
            kind: "custom".into(),
            data,
        };
        self.mode
            .handle_session_event(&AgentSessionEvent::EntryAppended { entry });
    }

    fn toggle(&mut self) -> Vec<(u16, bool)> {
        self.entries.0.lock().expect("entries lock").clear();
        self.mode.set_tools_expanded(!self.mode.tools_expanded);
        std::mem::take(&mut *self.entries.0.lock().expect("entries lock"))
    }
}

#[tokio::test]
async fn repeated_goal_updates_leave_one_visible_card_and_one_expansion_recipient() {
    let mut f = Fixture::new().await;
    for n in 0..=512 {
        f.append("goal", "same", n);
    }
    let paint = f.mode.render(100).join("\n");
    let calls = f.toggle();
    assert_eq!(paint.matches("[card:").count(), 1);
    assert!(paint.contains("[card:512]"));
    eprintln!(
        "RP2_OWNER {}",
        serde_json::json!({"tag": "recipients", "argv": std::env::args().collect::<Vec<_>>(), "exe": std::env::current_exe().expect("current test executable"), "data": {"visible":paint.matches("[card:").count(),"last":paint.matches("[card:512]").count(),"recipients":calls,"expanded":f.mode.tools_expanded}})
    );
    assert_eq!(calls, [(512, f.mode.tools_expanded)]);
}

#[tokio::test]
async fn replacement_preserves_status_stream_position_and_rebuilt_dispatch() {
    let mut f = Fixture::new().await;
    f.append("goal", "left", 1);
    f.append("note", "left", 2); // Same goal but a different custom type is a barrier.
    f.append("goal", "left", 3);
    f.append("goal", "right", 4);
    f.mode
        .handle_session_event(&AgentSessionEvent::ContinuationError {
            error_message: "status-boundary".into(),
        });
    let assistant =
        maho_ai::providers::faux::faux_assistant_message("stream-tail", Default::default());
    let message = AgentMessage::Llm(maho_ai::types::Message::Assistant(Arc::new(assistant)));
    f.mode.handle_event(&AgentEvent::MessageStart {
        message: message.clone(),
        streaming: None,
    });
    f.append("goal", "right", 5); // Skip the transient spacer/status before the streaming card.
    f.append("goal", "another", 6); // Insert before the streaming card, after the status.
    f.mode.handle_event(&AgentEvent::MessageEnd { message });
    let paint = f.mode.render(100).join("\n");
    assert!(!paint.contains("[card:4]"));
    let mut rest = paint.as_str();
    for marker in [
        "[card:1]",
        "[card:2]",
        "[card:3]",
        "[card:5]",
        "status-boundary",
        "[card:6]",
        "stream-tail",
    ] {
        rest = rest.split_once(marker).expect("live transcript order").1;
    }
    let calls = f.toggle();
    assert_eq!(calls, [1, 2, 3, 5, 6].map(|n| (n, f.mode.tools_expanded)));
    f.mode.rebuild_history();
    let rebuilt = f.toggle();
    eprintln!(
        "RP2_OWNER {}",
        serde_json::json!({"tag": "card-order", "argv": std::env::args().collect::<Vec<_>>(), "exe": std::env::current_exe().expect("current test executable"), "data": {"offsets":(["[card:1]","[card:2]","[card:3]","[card:5]","status-boundary","[card:6]","stream-tail"].map(|s|paint.find(s))),"retired":paint.find("[card:4]"),"calls":calls,"rebuilt":rebuilt}})
    );
    assert_eq!(rebuilt, [1, 2, 3, 5, 6].map(|n| (n, f.mode.tools_expanded)));
}
