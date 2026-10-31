use maho_ext_history_search::{
    filter::filter_history, indexer::index_sessions, resolve_search_root, types::HistoryEntry,
};
use serde_json::json;
use std::path::Path;
fn entry(text: &str, time: i64) -> HistoryEntry {
    HistoryEntry {
        text: text.into(),
        timestamp: time,
        session_id: "s".into(),
        session_file: "s.jsonl".into(),
        cwd: "/repo".into(),
    }
}
fn write(root: &Path, name: &str, texts: &[(&str, i64)]) {
    std::fs::create_dir_all(root).expect("mkdir");
    let mut lines = vec![json!({"type":"session","id":name,"cwd":"/repo"}).to_string()];
    for (text, time) in texts {
        lines.push(json!({"type":"message","timestamp":chrono::DateTime::from_timestamp_millis(*time).expect("timestamp").to_rfc3339(),"message":{"role":"user","content":[{"type":"text","text":text}]}}).to_string());
    }
    std::fs::write(root.join(name), lines.join("\n")).expect("fixture");
}
#[test]
fn missing_and_empty() {
    let d = tempfile::tempdir().expect("dir");
    assert!(
        index_sessions(&d.path().join("missing"))
            .expect("index")
            .is_empty()
    );
    assert!(index_sessions(d.path()).expect("index").is_empty());
}
#[test]
fn single_prompt() {
    let d = tempfile::tempdir().expect("dir");
    write(d.path(), "s.jsonl", &[("ship it", 2000)]);
    let r = index_sessions(d.path()).expect("index");
    assert_eq!(r[0].text, "ship it");
    assert_eq!(r[0].timestamp, 2000);
}
#[test]
fn injected_ignored() {
    let d = tempfile::tempdir().expect("dir");
    write(
        d.path(),
        "s.jsonl",
        &[
            ("[SYSTEM DIRECTIVE: hidden]", 1),
            ("[system:foo]", 2),
            ("[SYSTEM hidden]", 3),
            (" \n\t", 4),
            ("visible", 5),
        ],
    );
    assert_eq!(
        index_sessions(d.path())
            .expect("index")
            .iter()
            .map(|entry| entry.text.as_str())
            .collect::<Vec<_>>(),
        vec!["visible"]
    );
}
#[test]
fn newest_deduplication() {
    let d = tempfile::tempdir().expect("dir");
    write(d.path(), "old.jsonl", &[("duplicate", 2)]);
    write(d.path(), "new.jsonl", &[("duplicate", 4), ("latest", 5)]);
    let r = index_sessions(d.path()).expect("index");
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].text, "latest");
    assert_eq!(r[1].session_id, "new.jsonl");
}
#[test]
fn cap_keeps_recent() {
    let d = tempfile::tempdir().expect("dir");
    let strings: Vec<String> = (0..10005).map(|n| format!("prompt {n}")).collect();
    let texts: Vec<(&str, i64)> = strings
        .iter()
        .enumerate()
        .map(|(i, text)| (text.as_str(), i64::try_from(i).expect("small index")))
        .collect();
    write(d.path(), "bulk.jsonl", &texts);
    let r = index_sessions(d.path()).expect("index");
    assert_eq!(r.len(), 10000);
    assert_eq!(r[0].text, "prompt 10004");
    assert!(!r.iter().any(|e| e.text == "prompt 0"));
}
#[test]
fn legacy_string() {
    let d = tempfile::tempdir().expect("dir");
    let text=[json!({"type":"session","id":"legacy"}).to_string(),json!({"type":"message","timestamp":"2026-05-20T00:00:00Z","message":{"role":"user","content":"legacy string prompt"}}).to_string()].join("\n");
    std::fs::write(d.path().join("legacy.jsonl"), text).expect("fixture");
    assert_eq!(
        index_sessions(d.path()).expect("index")[0].text,
        "legacy string prompt"
    );
}
#[test]
fn top_level() {
    let d = tempfile::tempdir().expect("dir");
    write(d.path(), "flat.jsonl", &[("flat", 1)]);
    assert_eq!(index_sessions(d.path()).expect("index")[0].text, "flat");
}
#[test]
fn cwd_subdirectories() {
    let d = tempfile::tempdir().expect("dir");
    write(&d.path().join("aaa"), "20260101_old.jsonl", &[("old", 1)]);
    write(&d.path().join("zzz"), "20260901_new.jsonl", &[("fresh", 2)]);
    assert_eq!(index_sessions(d.path()).expect("index")[0].text, "fresh");
}
#[test]
fn filter_empty_and_case() {
    let entries = vec![
        entry("Newest prompt", 3),
        entry("Deploy production", 2),
        entry("older", 1),
    ];
    assert_eq!(filter_history(&entries, ""), entries);
    assert_eq!(
        filter_history(&entries, "DProd")[0].text,
        "Deploy production"
    );
}
#[test]
fn filter_tighter() {
    let entries = vec![entry("deploy dev prod", 2), entry("deploy production", 1)];
    assert_eq!(
        filter_history(&entries, "dprod")[0].text,
        "deploy production"
    );
}
fn fixture_path(path: &str) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        std::path::PathBuf::from(format!("C:{path}"))
    }
    #[cfg(not(windows))]
    {
        std::path::PathBuf::from(path)
    }
}
#[test]
fn root_empty() {
    assert_eq!(
        resolve_search_root(Path::new(""), &fixture_path("/sessions")),
        fixture_path("/sessions")
    );
}
#[test]
fn root_same() {
    assert_eq!(
        resolve_search_root(&fixture_path("/sessions"), &fixture_path("/sessions")),
        fixture_path("/sessions")
    );
}
#[test]
fn root_descendant() {
    assert_eq!(
        resolve_search_root(&fixture_path("/sessions/cwd"), &fixture_path("/sessions")),
        fixture_path("/sessions")
    );
}
#[test]
fn root_custom() {
    assert_eq!(
        resolve_search_root(&fixture_path("/custom"), &fixture_path("/sessions")),
        fixture_path("/custom")
    );
}
#[test]
fn root_windows() {
    let root = "C:\\Users\\u\\.senpi\\agent\\sessions";
    assert_eq!(
        maho_ext_history_search::resolve_search_root_windows(&format!("{root}\\encoded-cwd"), root),
        root
    );
    assert_eq!(
        maho_ext_history_search::resolve_search_root_windows("D:\\other\\sessions", root),
        "D:\\other\\sessions"
    );
}

mod command {
    use maho_ext_api::*;
    use maho_ext_history_search::HistorySearch;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    struct Session;
    impl ToolSessionManager for Session {
        fn session_id(&self) -> &str {
            "s"
        }
        fn session_file(&self) -> Option<&std::path::Path> {
            None
        }
    }
    impl SessionManager for Session {
        fn get_entries(&self) -> Vec<SessionEntry> {
            Vec::new()
        }
        fn get_branch(&self) -> Vec<SessionEntry> {
            Vec::new()
        }
        fn get_leaf_id(&self) -> Option<String> {
            None
        }
        fn get_session_name(&self) -> Option<String> {
            None
        }
    }
    struct Registry;
    impl ModelRegistry for Registry {
        fn get_all(&self) -> Vec<Model> {
            Vec::new()
        }
        fn get_available(&self) -> Vec<Model> {
            Vec::new()
        }
        fn find(&self, _: &str, _: &str) -> Option<Model> {
            None
        }
        fn has_configured_auth(&self, _: &Model) -> bool {
            false
        }
        fn get_api_key_for_provider<'a>(
            &'a self,
            _: &'a str,
        ) -> ExtensionFuture<'a, Option<String>> {
            Box::pin(async { Ok(None) })
        }
    }
    #[derive(Default)]
    struct Ui {
        notices: Mutex<Vec<(String, NotificationType)>>,
        editor: Mutex<Option<String>>,
    }
    impl ExtensionUi for Ui {
        fn select<'a>(
            &'a self,
            _: &'a str,
            _: &'a [String],
            _: ExtensionUiDialogOptions,
        ) -> UiFuture<'a, Option<String>> {
            Box::pin(async { None })
        }
        fn confirm<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
            _: ExtensionUiDialogOptions,
        ) -> UiFuture<'a, bool> {
            Box::pin(async { false })
        }
        fn input<'a>(
            &'a self,
            _: &'a str,
            _: Option<&'a str>,
            _: ExtensionUiDialogOptions,
        ) -> UiFuture<'a, Option<String>> {
            Box::pin(async { None })
        }
        fn notify(&self, message: &str, kind: NotificationType) {
            self.notices.lock().unwrap().push((message.into(), kind));
        }
        fn set_status(&self, _: &str, _: Option<&str>) {}
        fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
        fn set_header(&self, _: Option<ComponentFactory>) {}
        fn set_footer(&self, _: Option<ComponentFactory>) {}
        fn set_title(&self, _: &str) {}
        fn paste_to_editor(&self, _: &str) {}
        fn set_editor_text(&self, text: &str) {
            *self.editor.lock().unwrap() = Some(text.into());
        }
        fn get_editor_text(&self) -> String {
            String::new()
        }
        fn custom(
            &self,
            _: ComponentFactory,
            _: CustomUiOptions,
        ) -> ExtensionFuture<'_, JsonValue> {
            Box::pin(async { Err("UI unavailable".into()) })
        }
        fn theme(&self) -> Theme {
            Theme::default()
        }
    }
    fn context(ui: Arc<Ui>, has_ui: bool) -> ExtensionContext {
        ExtensionContext {
            ui,
            mode: ExtensionMode::Print,
            has_ui,
            cwd: "/repo".into(),
            agent_dir: "/agent".into(),
            session_manager: Arc::new(Session),
            model_registry: Arc::new(Registry),
            model: None,
            thinking_level: None,
            service_tier: None,
            effective_service_tier: None,
            scoped_models: Vec::new(),
            goal_store_file: None,
            loaded_extension_paths: Vec::new(),
            signal: None,
            steering_signal: None,
            is_idle_fn: Arc::new(|| true),
            wait_for_idle_fn: Arc::new(|| Box::pin(async {})),
            is_project_trusted_fn: Arc::new(|| true),
            is_compacting_fn: Arc::new(|| false),
            get_system_prompt_fn: Arc::new(String::new),
            get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default),
            registered_mcp_servers: Vec::new(),
            update_tool_hook_status: None,
            idle_coordinator: None,
            logger: None,
            defer_macrotask: None,
            compaction_signal: Default::default(),
        }
    }
    fn registered(selected: Arc<AtomicBool>, root: std::path::PathBuf) -> RegisteredCommand {
        let mut api = ExtensionApi::new(
            LoadedExtension::new("history-search", "/repo".into(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            ExtensionRuntime::default(),
        );
        let session_root = root.clone();
        HistorySearch {
            session_dir: Arc::new(move |_| Ok(session_root.clone())),
            default_sessions_root: Arc::new(move || root.clone()),
            select: Arc::new(move |_, entries| {
                selected.store(true, Ordering::SeqCst);
                Box::pin(async move { Ok(entries.into_iter().next()) })
            }),
        }
        .register(&mut api);
        api.registered
            .commands
            .iter()
            .find(|command| command.name == "history")
            .cloned()
            .expect("history command")
    }

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn registers_history_and_handles_no_ui_command_execution() {
        let root = tempfile::tempdir().unwrap();
        let selected = Arc::new(AtomicBool::new(false));
        let command = registered(selected.clone(), root.path().into());
        assert_eq!(
            command.description.as_deref(),
            Some("Search prompt history across sessions")
        );
        let ui = Arc::new(Ui::default());
        block_on((command.handler)("", &context(ui.clone(), false))).unwrap();
        assert_eq!(
            ui.notices.lock().unwrap().clone(),
            vec![("No UI available".to_owned(), NotificationType::Info)]
        );
        assert!(!selected.load(Ordering::SeqCst));
        assert!(ui.editor.lock().unwrap().is_none());
    }

    #[test]
    fn empty_history_notifies_and_selection_fills_the_editor() {
        let root = tempfile::tempdir().unwrap();
        let selected = Arc::new(AtomicBool::new(false));
        let command = registered(selected.clone(), root.path().into());
        let ui = Arc::new(Ui::default());
        block_on((command.handler)("", &context(ui.clone(), true))).unwrap();
        assert_eq!(
            ui.notices.lock().unwrap().clone(),
            vec![("No prompt history found".to_owned(), NotificationType::Info)]
        );
        super::write(root.path(), "s.jsonl", &[("ship it", 2000)]);
        block_on((command.handler)("", &context(ui.clone(), true))).unwrap();
        assert!(selected.load(Ordering::SeqCst));
        assert_eq!(ui.editor.lock().unwrap().as_deref(), Some("ship it"));
    }
}
