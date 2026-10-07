#![allow(dead_code)]
use maho_ext_api::*;

use std::{
    path::{Path, PathBuf},
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

#[derive(Default)]
pub struct RecordingUi {
    pub notices: Mutex<Vec<(String, NotificationType)>>,
    pub autocomplete_factories: Mutex<Vec<AutocompleteProviderFactory>>,
}

impl RecordingUi {
    pub fn notices(&self) -> Vec<(String, NotificationType)> {
        self.notices.lock().expect("notices").clone()
    }
    pub fn autocomplete_factory_count(&self) -> usize {
        self.autocomplete_factories.lock().expect("autocomplete factories").len()
    }
}

impl ExtensionUi for RecordingUi {
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, message: &str, kind: NotificationType) {
        self.notices.lock().expect("notices").push((message.to_owned(), kind));
    }
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
    fn add_autocomplete_provider(&self, factory: AutocompleteProviderFactory) -> Result<(), ExtensionFailure> {
        self.autocomplete_factories.lock().expect("autocomplete factories").push(factory);
        Ok(())
    }
}

#[derive(Default)]
pub struct RecordingActions(pub Mutex<Vec<CustomMessage>>);
impl ExtensionActions for RecordingActions {
    fn send_message(&self, message: CustomMessage, _: SendMessageOptions) -> Result<(), ExtensionFailure> {
        self.0.lock().expect("messages").push(message);
        Ok(())
    }
    fn send_user_message(&self, _: UserMessageContent, _: SendUserMessageOptions) -> Result<(), ExtensionFailure> {
        Err(ExtensionFailure::new("unexpected send_user_message"))
    }
    fn append_entry(&self, _: &str, _: Option<JsonValue>) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> {
        Ok(Vec::new())
    }
}

pub struct FakeSessionActions {
    pub commands: Vec<SlashCommandInfo>,
}

impl ExtensionSessionActions for FakeSessionActions {
    fn set_session_name(&self, _: &str) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn get_session_name(&self) -> Result<Option<String>, ExtensionFailure> {
        Ok(None)
    }
    fn set_label(&self, _: &str, _: Option<&str>) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn execute_tool<'a>(&'a self, _: &'a str, _: JsonValue, _: ExecuteToolOptions) -> ExecuteToolFuture<'a> {
        Box::pin(async {
            Err(ExecuteToolError {
                code: ExecuteToolErrorCode::UnknownTool,
                tool_name: String::new(),
                message: "unexpected execute_tool".to_owned(),
                active_tools: Vec::new(),
            })
        })
    }
    fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure> {
        Ok(Vec::new())
    }
    fn set_active_tools(&self, _: Vec<String>) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn refresh_tools(&self) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn register_removed_tool_hint(&self, _: &str, _: &str) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn register_lazy_tool_activator(&self, _: LazyToolActivator) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn get_commands(&self) -> Result<Vec<SlashCommandInfo>, ExtensionFailure> {
        Ok(self.commands.clone())
    }
    fn set_model(&self, _: Model) -> ExtensionFuture<'_, bool> {
        Box::pin(async { Ok(false) })
    }
    fn get_thinking_level(&self) -> Result<ThinkingLevel, ExtensionFailure> {
        Err(ExtensionFailure::new("unexpected get_thinking_level"))
    }
    fn set_thinking_level(&self, _: ThinkingLevel) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn set_session_model(&self, _: Model) -> ExtensionFuture<'_, bool> {
        Box::pin(async { Ok(false) })
    }
    fn set_session_thinking_level(&self, _: ThinkingLevel) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn set_session_fast_mode(&self, _: bool) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn exec<'a>(&'a self, _: &'a str, _: &'a [String], _: &'a Path, _: ExecOptions) -> ExtensionFuture<'a, ExecResult> {
        Box::pin(async { Err(ExtensionFailure::new("unexpected exec")) })
    }
}

pub fn context(cwd: &Path, ui: Arc<RecordingUi>, has_ui: bool) -> ExtensionContext {
    ExtensionContext {
        ui,
        mode: ExtensionMode::Print,
        has_ui,
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
        logger: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

pub struct SkillsFixture {
    _root: tempfile::TempDir,
    pub skills: PathBuf,
}

pub fn skills_fixture(names: &[&str]) -> SkillsFixture {
    let root = tempfile::tempdir().expect("temp");
    let skills = root.path().join("skills");
    for name in names {
        let dir = skills.join(name);
        std::fs::create_dir_all(&dir).expect("skill dir");
        std::fs::write(dir.join("SKILL.md"), format!("---\nname: {name}\ndescription: fixture\n---\nbody\n")).expect("skill file");
    }
    std::fs::create_dir_all(skills.join("not-a-skill")).expect("plain directory");
    SkillsFixture { _root: root, skills }
}

pub fn command(name: &str, description: Option<&str>, source: &str) -> SlashCommandInfo {
    SlashCommandInfo {
        name: name.to_owned(),
        description: description.map(str::to_owned),
        argument_hint: None,
        source_info: Some(SourceInfo { path: String::new(), source: source.to_owned(), ..Default::default() }),
    }
}
