#![allow(dead_code)]
use maho_ext_api::*;

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

struct TestSession;
impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str {
        "session"
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}
impl SessionManager for TestSession {
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

struct TestRegistry;
impl ModelRegistry for TestRegistry {
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
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}

pub struct TestUi;
impl ExtensionUi for TestUi {
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err("UI not available".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}

#[derive(Default)]
pub struct RecordingLogger(Mutex<Vec<(String, Option<JsonValue>)>>);
impl RecordingLogger {
    pub fn messages(&self) -> Vec<(String, Option<JsonValue>)> {
        self.0.lock().expect("log messages").clone()
    }
}
impl ComponentLogger for RecordingLogger {
    fn info(&self, message: &str, details: Option<&JsonValue>) {
        self.0.lock().expect("log messages").push((message.to_owned(), details.cloned()));
    }
    fn warn(&self, message: &str, details: Option<&JsonValue>) {
        self.0.lock().expect("log messages").push((message.to_owned(), details.cloned()));
    }
    fn error(&self, message: &str, details: Option<&JsonValue>) {
        self.0.lock().expect("log messages").push((message.to_owned(), details.cloned()));
    }
}

pub fn context(cwd: &Path, logger: Option<Arc<dyn ComponentLogger>>) -> ExtensionContext {
    ExtensionContext {
        ui: Arc::new(TestUi),
        mode: ExtensionMode::Print,
        has_ui: false,
        cwd: cwd.to_path_buf(),
        agent_dir: cwd.to_path_buf(),
        session_manager: Arc::new(TestSession),
        model_registry: Arc::new(TestRegistry),
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
        get_system_prompt_fn: Arc::new(|| "base".into()),
        get_system_prompt_options_fn: Arc::new(|| BuildSystemPromptOptions { cwd: "/tmp".into(), ..Default::default() }),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        idle_coordinator: None,
        logger,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

pub fn write_skill(skills_dir: &Path, name: &str) {
    let dir = skills_dir.join(name);
    std::fs::create_dir_all(&dir).expect("skill dir");
    std::fs::write(dir.join("SKILL.md"), format!("---\nname: {name}\ndescription: fixture\n---\nbody\n")).expect("skill file");
}

pub fn env_for_home(home: &Path) -> std::collections::BTreeMap<String, String> {
    std::collections::BTreeMap::from([("HOME".to_owned(), home.to_string_lossy().into_owned())])
}
