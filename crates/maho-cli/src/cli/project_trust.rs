use std::sync::Arc;

use maho_core::project_trust::{AppMode, ProjectTrustContext};
use maho_ext_api::{ExtensionContext, ExtensionFuture, ExtensionUi, ExtensionUiDialogOptions, NotificationType, ProjectTrustEventResult, UiFuture};
pub fn extension_mode(mode: AppMode) -> &'static str {
    match mode { AppMode::Interactive => "tui", AppMode::Print => "print", AppMode::Json => "json", AppMode::Rpc => "rpc" }
}
pub type Selector = dyn Fn(&str, &[String]) -> Option<String> + Send + Sync;
pub type Input = dyn Fn(&str, Option<&str>) -> Option<String> + Send + Sync;
pub struct CliProjectTrustContext {
    pub cwd: String,
    pub mode: AppMode,
    pub ui_available: bool,
    pub selector: Box<Selector>,
}
impl ProjectTrustContext for CliProjectTrustContext {
    fn has_ui(&self) -> bool { self.ui_available }
    fn select(&self, title: &str, options: &[String]) -> Option<String> {
        if !self.ui_available || self.mode != AppMode::Interactive { return None; }
        (self.selector)(title, options)
    }
}

fn to_extension_mode(mode: AppMode) -> maho_ext_api::ExtensionMode {
    match mode {
        AppMode::Interactive => maho_ext_api::ExtensionMode::Tui,
        AppMode::Print => maho_ext_api::ExtensionMode::Print,
        AppMode::Json => maho_ext_api::ExtensionMode::Json,
        AppMode::Rpc => maho_ext_api::ExtensionMode::Rpc,
    }
}

/// senpi `createProjectTrustContext` `ui`: dialogs answer only in an interactive UI, and notices
/// print to stderr outside the TUI.
pub struct ProjectTrustUi {
    pub mode: AppMode,
    pub has_ui: bool,
    pub select: Arc<Selector>,
    pub input: Arc<Input>,
}
impl ProjectTrustUi {
    fn prompts(&self) -> bool { self.has_ui && self.mode == AppMode::Interactive }
}
impl ExtensionUi for ProjectTrustUi {
    fn select<'a>(&'a self, title: &'a str, options: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        let selected = if self.prompts() { (self.select)(title, options) } else { None };
        Box::pin(async move { selected })
    }
    fn confirm<'a>(&'a self, title: &'a str, message: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> {
        let confirmed = self.prompts() && (self.select)(&format!("{title}\n{message}"), &["Yes".to_owned(), "No".to_owned()]).as_deref() == Some("Yes");
        Box::pin(async move { confirmed })
    }
    fn input<'a>(&'a self, title: &'a str, placeholder: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        let value = if self.prompts() { (self.input)(title, placeholder) } else { None };
        Box::pin(async move { value })
    }
    fn notify(&self, message: &str, kind: NotificationType) {
        if self.mode == AppMode::Interactive { return; }
        let color = match kind { NotificationType::Error => "\x1b[31m", NotificationType::Warning => "\x1b[33m", NotificationType::Info => "\x1b[36m" };
        maho_core::output_guard::maho_write_stderr(&format!("{color}{message}\x1b[39m\n"));
    }
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<maho_ext_api::WidgetContent>, _: maho_ext_api::ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String { String::new() }
    fn custom(&self, _: maho_ext_api::ComponentFactory, _: maho_ext_api::CustomUiOptions) -> ExtensionFuture<'_, serde_json::Value> {
        Box::pin(async { Ok(serde_json::Value::Null) })
    }
    fn theme(&self) -> maho_ext_api::Theme { Default::default() }
}

struct PreTrustSession;
impl maho_ext_api::ToolSessionManager for PreTrustSession {
    fn session_id(&self) -> &str { "" }
    fn session_file(&self) -> Option<&std::path::Path> { None }
}
impl maho_ext_api::SessionManager for PreTrustSession {
    fn get_entries(&self) -> Vec<maho_ext_api::SessionEntry> { Vec::new() }
    fn get_branch(&self) -> Vec<maho_ext_api::SessionEntry> { Vec::new() }
    fn get_leaf_id(&self) -> Option<String> { None }
    fn get_session_name(&self) -> Option<String> { None }
}

struct PreTrustModels;
impl maho_ext_api::ModelRegistry for PreTrustModels {
    fn get_all(&self) -> Vec<maho_ext_api::Model> { Vec::new() }
    fn get_available(&self) -> Vec<maho_ext_api::Model> { Vec::new() }
    fn find(&self, _: &str, _: &str) -> Option<maho_ext_api::Model> { None }
    fn has_configured_auth(&self, _: &maho_ext_api::Model) -> bool { false }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> { Box::pin(async { Ok(None) }) }
}

/// senpi `ProjectTrustContext` (`cwd`, `mode`, `hasUI`, `ui`) as the context the native
/// `project_trust` handlers receive; no session exists before trust is resolved.
pub fn project_trust_extension_context(cwd: &str, agent_dir: &str, mode: AppMode, ui: Arc<ProjectTrustUi>) -> ExtensionContext {
    ExtensionContext {
        has_ui: ui.has_ui, ui, mode: to_extension_mode(mode), cwd: cwd.into(), agent_dir: agent_dir.into(),
        session_manager: Arc::new(PreTrustSession), model_registry: Arc::new(PreTrustModels),
        model: None, thinking_level: None, service_tier: None, effective_service_tier: None, scoped_models: Vec::new(),
        goal_store_file: None, loaded_extension_paths: Vec::new(), signal: None, steering_signal: None,
        is_idle_fn: Arc::new(|| true), wait_for_idle_fn: Arc::new(|| Box::pin(async {})), is_project_trusted_fn: Arc::new(|| false),
        is_compacting_fn: Arc::new(|| false), get_system_prompt_fn: Arc::new(String::new),
        get_system_prompt_options_fn: Arc::new(maho_ext_api::BuildSystemPromptOptions::default),
        registered_mcp_servers: Vec::new(), update_tool_hook_status: None, idle_coordinator: None, logger: None,
        defer_macrotask: None, compaction_signal: Default::default(),
    }
}

/// senpi `emitProjectTrustEvent` over the pre-trust extension set: the first `yes`/`no` wins, and
/// each failing handler becomes `Extension "<path>" project_trust error: <error>`.
pub async fn emit_project_trust_event(
    loaded: maho_ext_host::loader::LoadExtensionsResult,
    context: ExtensionContext,
) -> (Option<ProjectTrustEventResult>, Vec<String>) {
    let cwd = context.cwd.clone();
    let mut runner = maho_ext_host::ExtensionRunner::new(loaded.extensions, loaded.runtime, loaded.events, context);
    let result = runner.emit_project_trust(cwd).await;
    let mut warnings: Vec<String> = runner.errors.iter()
        .map(|error| format!("Extension \"{}\" project_trust error: {}", error.extension_path, error.error)).collect();
    runner.invalidate("Project trust extensions are discarded after the trust decision");
    match result {
        Ok(result) => (result, warnings),
        Err(error) => {
            warnings.push(format!("Extension project_trust error: {}", error.message));
            (None, warnings)
        }
    }
}

/// senpi `DefaultResourceLoader.loadProjectTrustExtensions` (resource-loader.ts:605-611): the
/// builtin, inline, user/global and CLI extensions loaded with project settings forced untrusted,
/// so no project-local extension runs before the decision.
pub async fn load_project_trust_extensions(
    cwd: &str,
    agent_dir: &str,
    cli_extension_paths: &[String],
    no_extensions: bool,
) -> maho_ext_host::loader::LoadExtensionsResult {
    let mut settings = maho_core::settings_manager::SettingsManager::create(cwd, agent_dir, &maho_core::config::home_dir(), false);
    let (widget_sender, _widget_requests) = tokio::sync::mpsc::unbounded_channel();
    let factories = super::default_extensions::assembled_factories(widget_sender, Arc::new(std::sync::OnceLock::new()), &settings, std::path::Path::new(cwd), agent_dir);
    let (factories, discovery_errors) = match super::default_extensions::with_user_extensions(factories, &mut settings, cwd, agent_dir, cli_extension_paths, no_extensions).await {
        Ok(assembled) => assembled,
        Err(error) => (Vec::new(), vec![super::user_extensions::UserExtensionError {
            path: cwd.to_owned(), kind: super::user_extensions::UserExtensionErrorKind::UnknownNativeExtension, message: error,
        }]),
    };
    let mut loaded = maho_ext_host::loader::load_extensions_async(factories, std::path::Path::new(cwd), Default::default()).await;
    loaded.errors.extend(discovery_errors.into_iter().map(|error| maho_ext_api::ExtensionError {
        extension_path: error.path, event: "load".into(), error: error.message, stack: None,
    }));
    loaded
}

/// The pre-trust extensions' `project_trust` decision for one cwd, with the load failures and the
/// handler failures as separate warning lists (senpi reports both through `onExtensionError` and the
/// extension load diagnostics).
pub struct ExtensionProjectTrust {
    pub result: Option<ProjectTrustEventResult>,
    pub load_warnings: Vec<String>,
    pub trust_warnings: Vec<String>,
}

pub async fn resolve_extension_project_trust(
    cwd: &str,
    agent_dir: &str,
    cli_extension_paths: &[String],
    no_extensions: bool,
    mode: AppMode,
    ui: Arc<ProjectTrustUi>,
) -> ExtensionProjectTrust {
    let loaded = load_project_trust_extensions(cwd, agent_dir, cli_extension_paths, no_extensions).await;
    let load_warnings = loaded.errors.iter()
        .map(|error| super::default_extensions::extension_load_failure_message(&error.extension_path, &error.error)).collect();
    let (result, trust_warnings) = emit_project_trust_event(loaded, project_trust_extension_context(cwd, agent_dir, mode, ui)).await;
    ExtensionProjectTrust { result, load_warnings, trust_warnings }
}
