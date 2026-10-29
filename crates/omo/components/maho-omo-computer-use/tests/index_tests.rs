use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use maho_ext_api::{
    BuildSystemPromptOptions, ComponentFactory, CustomComponentFactory, CustomUiFactoryOptions,
    CustomUiOptions, EditMessageOptions, EditMessageResult, EventBus, EventKind, EventResult,
    ExtensionApi, ExtensionCommandContext, ExtensionCommandContextActions, ExtensionContext,
    ExtensionEvent, ExtensionFailure, ExtensionFuture, ExtensionMode, ExtensionRuntime,
    ExtensionSessionProfile, ExtensionTreeNavigationOptions, ExtensionUi, ExtensionUiDialogOptions,
    ExtensionUiFactories, ExtensionWidgetOptions, FooterComponentFactory, ForkOptions, JsonValue,
    LoadedExtension, Model, ModelRegistry, NewSessionOptions, NotificationType,
    ResourcesDiscoverEvent, SessionEntry, SessionManager, SessionNavigationResult, SessionReason,
    SlashCommandInfo, SourceInfo, SourceOrigin, SourceScope, SwitchSessionOptions, Theme,
    ThinkingLevel, ToolSessionManager, TuiComponentFactory, UiFuture, WidgetContent,
};
use maho_omo_computer_use::index::{COMPUTER_UNAVAILABLE, COMPUTER_USE_COMPONENT_NAME};
use maho_omo_computer_use::runtime_loader::{
    ComputerHostApi, ComputerRuntimeLoader, ComputerRuntimeLoaderOptions,
};
use maho_omo_computer_use::telemetry::{
    ComputerUseTelemetryObservation, ComputerUseTelemetryObservers,
    SharedComputerUseTelemetryObservers,
};
use maho_omo_computer_use::{ComputerUseComponent, ComputerUseComponentOptions};
use senpi_desktop_tool::registration::{
    COMPUTER_SKILL_NAME, COMPUTER_SUBCOMMANDS, is_supported_host,
};
use senpi_desktop_tool::session::{ComputerHostContext, SimpleSessionManager};
use senpi_desktop_tool::settings::{AuditLogSettings, ComputerSettings, ScreenshotGcSettings};
use serde_json::json;

type NotifyLog = Arc<std::sync::Mutex<Vec<(String, NotificationType)>>>;

fn fake_engine_child() -> maho_omo_computer_use::runtime_loader::EngineChildFactory {
    let fake_engine_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/omo/components parent")
        .parent()
        .expect("crates/omo parent")
        .join("senpi-desktop-service/tests/fixtures/fake_engine.py");
    Arc::new(move |_| {
        let p = fake_engine_path.clone();
        senpi_desktop_service::child::engine_child_factory(Some(
            senpi_desktop_service::child::EngineSpawnConfig {
                binary_path: PathBuf::from(if cfg!(windows) { "python" } else { "python3" }),
                args: vec![p.to_string_lossy().to_string(), "--stdio".to_string()],
                env: std::collections::HashMap::new(),
            },
        ))
    })
}

#[derive(Default)]
struct TestHostApi {
    tools: std::sync::Mutex<Vec<String>>,
}

impl ComputerHostApi for TestHostApi {
    fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure> {
        let tools = self.tools.lock().map(|t| t.clone()).unwrap_or_default();
        Ok(tools)
    }

    fn set_active_tools(&self, names: Vec<String>) -> Result<(), ExtensionFailure> {
        if let Ok(mut tools) = self.tools.lock() {
            *tools = names;
        }
        Ok(())
    }

    fn execute_tool(
        &self,
        _name: &str,
        _params: JsonValue,
        _signal: Option<maho_ai::utils::abort::AbortSignal>,
    ) -> futures::future::BoxFuture<'static, Result<JsonValue, String>> {
        Box::pin(async move { Ok(JsonValue::Null) })
    }
}

struct TestSessionActions {
    commands: Vec<SlashCommandInfo>,
    tools: std::sync::Mutex<Vec<String>>,
}

impl maho_ext_api::ExtensionSessionActions for TestSessionActions {
    fn set_session_name(&self, _: &str) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn get_session_name(&self) -> Result<Option<String>, ExtensionFailure> {
        Ok(None)
    }
    fn set_label(&self, _: &str, _: Option<&str>) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn execute_tool<'a>(
        &'a self,
        _: &'a str,
        _: JsonValue,
        _: maho_ext_api::ExecuteToolOptions,
    ) -> maho_ext_api::ExecuteToolFuture<'a> {
        Box::pin(async {
            Ok(maho_ext_api::AgentToolResult {
                content: vec![],
                details: JsonValue::Null,
                is_error: Some(false),
                terminate: Some(false),
                added_tool_names: Some(vec![]),
                structured_content: None,
                usage: None,
                nested_calls: None,
            })
        })
    }
    fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure> {
        Ok(self.tools.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }
    fn set_active_tools(&self, names: Vec<String>) -> Result<(), ExtensionFailure> {
        *self.tools.lock().unwrap_or_else(|e| e.into_inner()) = names;
        Ok(())
    }
    fn refresh_tools(&self) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn register_removed_tool_hint(&self, _: &str, _: &str) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn register_lazy_tool_activator(
        &self,
        _: maho_ext_api::LazyToolActivator,
    ) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn get_commands(&self) -> Result<Vec<SlashCommandInfo>, ExtensionFailure> {
        Ok(self.commands.clone())
    }
    fn set_model(&self, _: Model) -> ExtensionFuture<'_, bool> {
        Box::pin(async { Ok(true) })
    }
    fn get_thinking_level(&self) -> Result<ThinkingLevel, ExtensionFailure> {
        Ok(ThinkingLevel::Minimal)
    }
    fn set_thinking_level(&self, _: ThinkingLevel) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn set_session_model(&self, _: Model) -> ExtensionFuture<'_, bool> {
        Box::pin(async { Ok(true) })
    }
    fn set_session_thinking_level(&self, _: ThinkingLevel) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn set_session_fast_mode(&self, _: bool) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn exec<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: &'a Path,
        _: maho_ext_api::ExecOptions,
    ) -> ExtensionFuture<'a, maho_ext_api::ExecResult> {
        Box::pin(async {
            Ok(maho_ext_api::ExecResult {
                stdout: String::new(),
                stderr: String::new(),
                code: 0,
                killed: false,
            })
        })
    }
}

fn sample_settings(enabled: bool, cua_adapter: bool) -> ComputerSettings {
    ComputerSettings {
        enabled,
        cua_adapter,
        engine_path: None,
        max_width: 1920,
        max_height: 1080,
        screenshot_max_bytes: 1024 * 1024,
        stop_hotkey: "ctrl+c".to_string(),
        allow_host_relay_only_stop: true,
        macos_canary: String::new(),
        display: String::new(),
        audit_log: AuditLogSettings { enabled: false },
        screenshot_gc: ScreenshotGcSettings::default(),
    }
}

fn make_api() -> ExtensionApi {
    ExtensionApi::new(
        LoadedExtension::new(
            "test-computer-use",
            "/tmp".into(),
            SourceInfo {
                path: "/tmp".to_string(),
                source: "test".to_string(),
                scope: SourceScope::System,
                origin: SourceOrigin::TopLevel,
                base_dir: Some("/test".to_string()),
            },
        ),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    )
}

struct FauxSession;
impl ToolSessionManager for FauxSession {
    fn session_id(&self) -> &str {
        "test-session-123"
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}
impl SessionManager for FauxSession {
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
    fn get_session_dir(&self) -> Option<PathBuf> {
        Some(PathBuf::from("/tmp"))
    }
}

struct FauxRegistry;
impl ModelRegistry for FauxRegistry {
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
        true
    }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(Some("key".to_owned())) })
    }
    fn get_provider_auth<'a>(
        &'a self,
        _: &'a str,
    ) -> ExtensionFuture<'a, Option<maho_ai::models::AuthResolution>> {
        Box::pin(async {
            Ok(Some(maho_ai::models::AuthResolution {
                ambient: false,
                auth: maho_ai::models::ProviderAuthResult {
                    api_key: Some("key".to_owned()),
                    headers: None,
                    base_url: Some("http://127.0.0.1:8080".to_owned()),
                },
                env: None,
            }))
        })
    }
}

struct FauxFactories {
    custom_calls: AtomicUsize,
}
impl ExtensionUiFactories for FauxFactories {
    fn set_widget_factory(
        &self,
        _: &str,
        _: Option<TuiComponentFactory>,
        _: ExtensionWidgetOptions,
    ) {
    }
    fn set_header_factory(&self, _: Option<TuiComponentFactory>) {}
    fn set_footer_factory(&self, _: Option<FooterComponentFactory>) {}
    fn custom_factory(
        &self,
        _: CustomComponentFactory,
        _: CustomUiFactoryOptions,
    ) -> ExtensionFuture<'_, JsonValue> {
        self.custom_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(JsonValue::Null) })
    }
}

struct FauxUi {
    factories: FauxFactories,
    notifies: Arc<std::sync::Mutex<Vec<(String, NotificationType)>>>,
}
impl ExtensionUi for FauxUi {
    fn factories(&self) -> Option<&dyn ExtensionUiFactories> {
        Some(&self.factories)
    }
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
    fn notify(&self, message: &str, level: NotificationType) {
        if let Ok(mut list) = self.notifies.lock() {
            list.push((message.to_string(), level));
        }
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
        Box::pin(async { Err(ExtensionFailure::new("unavailable")) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}

struct FauxActions;
impl ExtensionCommandContextActions for FauxActions {
    fn wait_for_idle(&self) -> ExtensionFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn new_session(&self, _: NewSessionOptions) -> ExtensionFuture<'_, SessionNavigationResult> {
        Box::pin(async { Ok(SessionNavigationResult::default()) })
    }
    fn fork<'a>(
        &'a self,
        _: &'a str,
        _: ForkOptions,
    ) -> ExtensionFuture<'a, SessionNavigationResult> {
        Box::pin(async { Ok(SessionNavigationResult::default()) })
    }
    fn navigate_tree<'a>(
        &'a self,
        _: &'a str,
        _: ExtensionTreeNavigationOptions,
    ) -> ExtensionFuture<'a, SessionNavigationResult> {
        Box::pin(async { Ok(SessionNavigationResult::default()) })
    }
    fn edit_assistant_message<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: EditMessageOptions,
    ) -> ExtensionFuture<'a, EditMessageResult> {
        Box::pin(async { Ok(EditMessageResult::default()) })
    }
    fn edit_user_message<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: EditMessageOptions,
    ) -> ExtensionFuture<'a, EditMessageResult> {
        Box::pin(async { Ok(EditMessageResult::default()) })
    }
    fn switch_session<'a>(
        &'a self,
        _: &'a str,
        _: SwitchSessionOptions,
    ) -> ExtensionFuture<'a, SessionNavigationResult> {
        Box::pin(async { Ok(SessionNavigationResult::default()) })
    }
    fn reload(&self) -> ExtensionFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

fn command_context(dir: &Path, mode: ExtensionMode) -> (ExtensionCommandContext, NotifyLog) {
    let notifies = Arc::new(std::sync::Mutex::new(Vec::new()));
    let ui = Arc::new(FauxUi {
        factories: FauxFactories {
            custom_calls: AtomicUsize::new(0),
        },
        notifies: Arc::clone(&notifies),
    });
    let context = ExtensionContext {
        ui: ui.clone(),
        mode,
        has_ui: mode == ExtensionMode::Tui,
        cwd: dir.to_path_buf(),
        agent_dir: dir.to_path_buf(),
        session_manager: Arc::new(FauxSession),
        model_registry: Arc::new(FauxRegistry),
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
    };
    (
        ExtensionCommandContext {
            context,
            actions: Arc::new(FauxActions),
            runtime: ExtensionRuntime::default(),
        },
        notifies,
    )
}

#[test]
fn test_given_unsupported_host_when_registered_then_all_features_absent() {
    let mut api = make_api();
    let comp = ComputerUseComponent::new(ComputerUseComponentOptions {
        platform: Some("windows".to_string()),
        load_settings: Some(Arc::new(|_, _| Ok(sample_settings(true, true)))),
        ..Default::default()
    });
    comp.register(&mut api);

    assert!(api.registered.commands.iter().any(|c| c.name == "computer"));
    assert!(
        !api.registered
            .tools
            .iter()
            .any(|t| t.definition.name == "computer")
    );
    assert!(
        !api.registered
            .tools
            .iter()
            .any(|t| t.definition.name == "computer_actions")
    );
}

#[tokio::test]
async fn test_given_unsupported_host_when_command_runs_then_notifies_unavailable() {
    let mut api = make_api();
    let comp = ComputerUseComponent::new(ComputerUseComponentOptions {
        platform: Some("windows".to_string()),
        load_settings: Some(Arc::new(|_, _| Ok(sample_settings(true, true)))),
        ..Default::default()
    });
    comp.register(&mut api);

    let handler = api
        .registered
        .command_context_handlers
        .get("computer")
        .cloned()
        .expect("test value");
    let (ctx, notifies) = command_context(Path::new("/tmp"), ExtensionMode::Tui);
    handler("status", &ctx).await.expect("test value");

    let msgs = notifies.lock().expect("test value");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].0, COMPUTER_UNAVAILABLE);
    assert_eq!(msgs[0].1, NotificationType::Warning);
}

#[test]
fn test_given_supported_host_disabled_when_registered_then_only_command_registered() {
    let mut api = make_api();
    let comp = ComputerUseComponent::new(ComputerUseComponentOptions {
        platform: Some("linux".to_string()),
        load_settings: Some(Arc::new(|_, _| Ok(sample_settings(false, false)))),
        ..Default::default()
    });
    comp.register(&mut api);

    assert!(api.registered.commands.iter().any(|c| c.name == "computer"));
    assert!(
        !api.registered
            .tools
            .iter()
            .any(|t| t.definition.name == "computer")
    );
    assert!(
        !api.registered
            .tools
            .iter()
            .any(|t| t.definition.name == "computer_actions")
    );
}

#[test]
fn test_given_supported_host_enabled_without_cua_when_registered_then_only_computer_tool_present() {
    let mut api = make_api();
    let comp = ComputerUseComponent::new(ComputerUseComponentOptions {
        platform: Some("linux".to_string()),
        load_settings: Some(Arc::new(|_, _| Ok(sample_settings(true, false)))),
        ..Default::default()
    });
    comp.register(&mut api);

    assert!(api.registered.commands.iter().any(|c| c.name == "computer"));
    assert!(
        api.registered
            .tools
            .iter()
            .any(|t| t.definition.name == "computer")
    );
    assert!(
        !api.registered
            .tools
            .iter()
            .any(|t| t.definition.name == "computer_actions")
    );
}

#[test]
fn test_given_supported_host_enabled_with_cua_when_registered_then_both_tools_present() {
    let mut api = make_api();
    let comp = ComputerUseComponent::new(ComputerUseComponentOptions {
        platform: Some("linux".to_string()),
        load_settings: Some(Arc::new(|_, _| Ok(sample_settings(true, true)))),
        ..Default::default()
    });
    comp.register(&mut api);

    assert!(api.registered.commands.iter().any(|c| c.name == "computer"));
    assert!(
        api.registered
            .tools
            .iter()
            .any(|t| t.definition.name == "computer")
    );
    assert!(
        api.registered
            .tools
            .iter()
            .any(|t| t.definition.name == "computer_actions")
    );
}

#[test]
fn test_given_supported_host_enabled_tools_are_search_exposed() {
    let mut api = make_api();
    let comp = ComputerUseComponent::new(ComputerUseComponentOptions {
        platform: Some("linux".to_string()),
        load_settings: Some(Arc::new(|_, _| Ok(sample_settings(true, true)))),
        ..Default::default()
    });
    comp.register(&mut api);

    let tools = &api.registered.tools;
    assert_eq!(tools.len(), 2);
    for t in tools {
        assert!(!t.definition.name.is_empty());
    }
}

#[test]
fn test_given_command_registered_when_subcommands_completed_then_matches_parity() {
    let mut api = make_api();
    let comp = ComputerUseComponent::new(ComputerUseComponentOptions {
        platform: Some("linux".to_string()),
        load_settings: Some(Arc::new(|_, _| Ok(sample_settings(true, true)))),
        ..Default::default()
    });
    comp.register(&mut api);

    let cmd = api
        .registered
        .commands
        .iter()
        .find(|c| c.name == "computer")
        .expect("test value");
    assert_eq!(cmd.name, "computer");
    assert_eq!(
        cmd.description,
        Some("Computer use (experimental): on, off, status, stop, or resume (stop and resume are user-only)".to_string())
    );
    assert_eq!(cmd.argument_hint, Some(COMPUTER_SUBCOMMANDS.join("|")));
    assert!(
        api.registered
            .command_argument_completions
            .contains_key("computer")
    );
}

#[test]
fn test_given_invalid_settings_when_load_fails_then_behaves_as_disabled() {
    let mut api = make_api();
    let comp = ComputerUseComponent::new(ComputerUseComponentOptions {
        platform: Some("linux".to_string()),
        load_settings: Some(Arc::new(|_, _| Err("invalid schema".to_string()))),
        ..Default::default()
    });
    comp.register(&mut api);

    assert!(api.registered.commands.iter().any(|c| c.name == "computer"));
    assert!(
        !api.registered
            .tools
            .iter()
            .any(|t| t.definition.name == "computer")
    );
}

#[test]
fn test_given_custom_env_when_registered_then_used_for_ancestry_detection() {
    let mut api = make_api();
    let mut env = BTreeMap::new();
    env.insert("OMO_SENPI_TASK_RPC_CHILD".to_string(), "1".to_string());
    let comp = ComputerUseComponent::new(ComputerUseComponentOptions {
        platform: Some("linux".to_string()),
        env: Some(env),
        load_settings: Some(Arc::new(|_, _| Ok(sample_settings(true, false)))),
        ..Default::default()
    });
    comp.register(&mut api);
    assert!(
        api.registered
            .tools
            .iter()
            .any(|t| t.definition.name == "computer")
    );
}

#[test]
fn test_component_constant_name_parity() {
    assert_eq!(COMPUTER_USE_COMPONENT_NAME, "computer-use");
    assert_eq!(
        COMPUTER_UNAVAILABLE,
        "Computer use is unavailable in this session."
    );
    assert_eq!(COMPUTER_SKILL_NAME, "computer-use");
}

#[test]
fn test_computer_command_usage_parity() {
    assert_eq!(
        senpi_desktop_tool::registration::COMPUTER_COMMAND_USAGE,
        "Usage: /computer on|off|status|stop|resume"
    );
    assert_eq!(
        COMPUTER_SUBCOMMANDS,
        &["on", "off", "status", "stop", "resume"]
    );
}

#[test]
fn test_is_supported_host_parity() {
    assert!(is_supported_host("darwin"));
    assert!(is_supported_host("linux"));
    assert!(is_supported_host("win32"));
    assert!(!is_supported_host("windows"));
    assert!(!is_supported_host("freebsd"));
}

#[tokio::test]
async fn test_given_registered_component_when_only_startup_events_fire_then_loads_on_first_use() {
    let loaded = Arc::new(AtomicBool::new(false));
    let loaded_clone = Arc::clone(&loaded);

    let loader = ComputerRuntimeLoader::new(ComputerRuntimeLoaderOptions {
        host: Arc::new(TestHostApi::default()),
        settings: sample_settings(true, false),
        engine_child: None,
        on_engine_error: Arc::new(|_| {}),
        on_permission_required: None,
        load_runtime: Some(Arc::new(move || {
            let l = Arc::clone(&loaded_clone);
            Box::pin(async move {
                l.store(true, Ordering::SeqCst);
                Err("simulated first-use probe".to_string())
            })
        })),
    });

    assert!(loader.started().is_none());
    assert!(!loaded.load(Ordering::SeqCst));

    let _ = loader.load().await;
    assert!(loaded.load(Ordering::SeqCst));
}

#[test]
fn test_given_runtime_never_loaded_when_session_shuts_down_then_stays_unloaded() {
    let loader = ComputerRuntimeLoader::new(ComputerRuntimeLoaderOptions {
        host: Arc::new(TestHostApi::default()),
        settings: sample_settings(true, false),
        engine_child: None,
        on_engine_error: Arc::new(|_| {}),
        on_permission_required: None,
        load_runtime: None,
    });
    assert!(loader.started().is_none());
}

#[test]
fn test_given_enabled_host_when_resources_discover_fires_then_skill_contributed() {
    let skill = maho_omo_bundled_skills::contributed_skill::resolve_contributed_skill(
        maho_omo_bundled_skills::contributed_skill::ResolveContributedSkillOptions {
            commands: None,
            name: COMPUTER_SKILL_NAME,
            path: std::path::PathBuf::from("/tmp/fake-skill"),
            cwd: std::path::PathBuf::from("/tmp"),
            env: &BTreeMap::new(),
        },
    );
    assert!(matches!(
        skill,
        maho_omo_bundled_skills::contributed_skill::ContributedSkill::Contributed { .. }
    ));
}

#[tokio::test]
async fn test_given_status_command_when_executed_then_reports_engine_prelude_source() {
    let mut api = make_api();
    let comp = ComputerUseComponent::new(ComputerUseComponentOptions {
        platform: Some("linux".to_string()),
        load_settings: Some(Arc::new(|_, _| Ok(sample_settings(true, false)))),
        ..Default::default()
    });
    comp.register(&mut api);

    let handler = api
        .registered
        .command_context_handlers
        .get("computer")
        .cloned()
        .expect("test value");
    let (ctx, notifies) = command_context(Path::new("/tmp"), ExtensionMode::Tui);
    handler("status", &ctx).await.expect("test value");

    let msgs = notifies.lock().expect("test value");
    assert_eq!(msgs.len(), 1);
    let status_text = &msgs[0].0;
    assert!(status_text.contains("Computer use: enabled=true"));
    assert!(status_text.contains("engine: not started"));
    assert!(status_text.contains("prelude: inactive"));
    assert!(status_text.contains("engine source:"));
}

#[tokio::test]
async fn test_given_disabled_skills_names_computer_use_then_no_skill_contributed() {
    let dir = tempfile::tempdir().expect("test value");
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join(".omo")).expect("test value");
    std::fs::write(
        home.join(".omo/omo.jsonc"),
        "{\"disabled_skills\":[\"computer-use\"]}",
    )
    .expect("config");
    let mut env = BTreeMap::new();
    env.insert("HOME".to_string(), home.to_string_lossy().to_string());
    env.insert(
        "USERPROFILE".to_string(),
        home.to_string_lossy().to_string(),
    );
    let skill = maho_omo_bundled_skills::contributed_skill::resolve_contributed_skill(
        maho_omo_bundled_skills::contributed_skill::ResolveContributedSkillOptions {
            commands: None,
            name: COMPUTER_SKILL_NAME,
            path: std::path::PathBuf::from("/tmp/fake-skill"),
            cwd: dir.path().to_path_buf(),
            env: &env,
        },
    );
    assert!(matches!(
        skill,
        maho_omo_bundled_skills::contributed_skill::ContributedSkill::Disabled
    ));
}

#[tokio::test]
async fn test_given_cua_adapter_activation_changes_update_host_active_tools() {
    let host = Arc::new(TestHostApi::default());
    let _ = host.set_active_tools(vec!["read".to_string()]);

    let loader = ComputerRuntimeLoader::new(ComputerRuntimeLoaderOptions {
        host: Arc::clone(&host) as Arc<dyn ComputerHostApi>,
        settings: sample_settings(true, true),
        engine_child: Some(fake_engine_child()),
        on_engine_error: Arc::new(|_| {}),
        on_permission_required: None,
        load_runtime: None,
    });

    let rt = loader.load().await.expect("test value");
    assert_eq!(
        host.get_active_tools().expect("test value"),
        vec!["read".to_string()]
    );

    let host_ctx = ComputerHostContext {
        cwd: PathBuf::from("/tmp"),
        session_manager: Arc::new(SimpleSessionManager {
            session_id: "s1".to_string(),
            session_dir: PathBuf::from("/tmp"),
        }),
        model: None,
    };

    let _ = rt.handle.activate(&host_ctx).await;
    let active = host.get_active_tools().expect("test value");
    assert!(active.contains(&"computer".to_string()));
    assert!(active.contains(&"computer_actions".to_string()));

    let _ = rt.handle.deactivate().await;
    let inactive = host.get_active_tools().expect("test value");
    assert!(!inactive.contains(&"computer".to_string()));
    assert!(!inactive.contains(&"computer_actions".to_string()));
}

#[tokio::test]
async fn test_given_no_cua_adapter_activation_changes_only_activates_computer() {
    let host = Arc::new(TestHostApi::default());
    let _ = host.set_active_tools(vec!["read".to_string()]);

    let loader = ComputerRuntimeLoader::new(ComputerRuntimeLoaderOptions {
        host: Arc::clone(&host) as Arc<dyn ComputerHostApi>,
        settings: sample_settings(true, false),
        engine_child: Some(fake_engine_child()),
        on_engine_error: Arc::new(|_| {}),
        on_permission_required: None,
        load_runtime: None,
    });

    let rt = loader.load().await.expect("test value");
    let host_ctx = ComputerHostContext {
        cwd: PathBuf::from("/tmp"),
        session_manager: Arc::new(SimpleSessionManager {
            session_id: "s2".to_string(),
            session_dir: PathBuf::from("/tmp"),
        }),
        model: None,
    };

    let _ = rt.handle.activate(&host_ctx).await;
    let active = host.get_active_tools().expect("test value");
    assert!(active.contains(&"computer".to_string()));
    assert!(!active.contains(&"computer_actions".to_string()));
}

#[tokio::test]
async fn test_given_telemetry_observers_when_tool_activated_and_command_off_runs_both_transitions_fire()
 {
    let obs = Arc::new(SharedComputerUseTelemetryObservers::new());
    let (sender, mut received) = tokio::sync::mpsc::unbounded_channel();

    // Subscribe before triggering so the spawned activation cannot be missed.
    let _unsub = obs.subscribe(Arc::new(move |item| {
        let _ = sender.send(item);
    }));

    let mut api = make_api();
    let comp = ComputerUseComponent::new(ComputerUseComponentOptions {
        platform: Some("linux".to_string()),
        load_settings: Some(Arc::new(|_, _| Ok(sample_settings(true, false)))),
        telemetry_observers: Some(obs),
        engine_child: Some(fake_engine_child()),
        ..Default::default()
    });
    comp.register(&mut api);

    // Context from SessionStart
    api.events
        .emit("session_start", &json!({ "session_id": "telem-session" }));

    // Trigger tool activated via registered event bus boundary
    api.events
        .emit("tool_activated", &json!({ "toolNames": ["computer"] }));

    // The activation runs on a spawned task: await the exact observation, bounded.
    let activated = tokio::time::timeout(tokio::time::Duration::from_secs(5), async {
        while let Some(event) = received.recv().await {
            if matches!(
                event,
                ComputerUseTelemetryObservation::Activation { active: true, .. }
            ) {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(activated);
}

#[tokio::test]
async fn test_given_another_tool_activated_then_no_activation_emitted() {
    let obs = Arc::new(SharedComputerUseTelemetryObservers::new());
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = Arc::clone(&events);

    let _unsub = obs.subscribe(Arc::new(move |item| {
        events_clone.lock().expect("test value").push(item);
    }));

    let mut api = make_api();
    let comp = ComputerUseComponent::new(ComputerUseComponentOptions {
        platform: Some("linux".to_string()),
        load_settings: Some(Arc::new(|_, _| Ok(sample_settings(true, false)))),
        telemetry_observers: Some(obs),
        ..Default::default()
    });
    comp.register(&mut api);

    api.events
        .emit("session_start", &json!({ "session_id": "telem-session" }));

    // Trigger tool activated for another tool via registered event bus boundary
    api.events
        .emit("tool_activated", &json!({ "toolNames": ["read", "bash"] }));

    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let evs = events.lock().expect("test value").clone();
    assert!(evs.is_empty());
}

#[tokio::test]
async fn test_given_user_skill_registered_when_resources_discover_fires_then_built_in_yields() {
    let mut api = make_api();
    let comp = ComputerUseComponent::new(ComputerUseComponentOptions {
        platform: Some("linux".to_string()),
        load_settings: Some(Arc::new(|_, _| Ok(sample_settings(true, false)))),
        ..Default::default()
    });
    comp.register(&mut api);

    let handler = api
        .registered
        .handlers
        .get(&EventKind::ResourcesDiscover)
        .expect("handler registered")[0]
        .clone();
    let temp = tempfile::tempdir().expect("tempdir");
    let (ctx, _notifies) = command_context(temp.path(), ExtensionMode::Tui);

    // Execute through registered ResourcesDiscover handler
    let mut evt = ExtensionEvent::ResourcesDiscover(ResourcesDiscoverEvent {
        cwd: temp.path().to_path_buf(),
        reason: SessionReason::Startup,
        scoped_entries: false,
    });
    let res = handler(&mut evt, &ctx).await.expect("success");
    let EventResult::ResourcesDiscover(discovered) = res else {
        panic!("expected resources discover")
    };
    assert_eq!(discovered.skill_paths.len(), 1);
}

#[tokio::test]
async fn test_given_user_skill_when_status_runs_then_names_user_skill_in_its_place() {
    let mut api = make_api();
    let user_skill_path = "/home/me/.omo/agent/skills/computer-use/SKILL.md".to_string();
    let actions = Arc::new(TestSessionActions {
        commands: vec![SlashCommandInfo {
            name: "skill:computer-use".to_string(),
            description: None,
            argument_hint: None,
            source_info: Some(SourceInfo {
                path: user_skill_path.clone(),
                source: "skill".to_string(),
                scope: SourceScope::User,
                origin: SourceOrigin::TopLevel,
                base_dir: None,
            }),
        }],
        tools: std::sync::Mutex::new(vec![]),
    });
    api.runtime.bind_session_actions(actions);

    let comp = ComputerUseComponent::new(ComputerUseComponentOptions {
        platform: Some("linux".to_string()),
        load_settings: Some(Arc::new(|_, _| Ok(sample_settings(true, false)))),
        ..Default::default()
    });
    comp.register(&mut api);

    let handler = api
        .registered
        .handlers
        .get(&EventKind::ResourcesDiscover)
        .expect("handler registered")[0]
        .clone();
    let temp = tempfile::tempdir().expect("tempdir");
    let (ctx, notifies) = command_context(temp.path(), ExtensionMode::Tui);

    let mut evt = ExtensionEvent::ResourcesDiscover(ResourcesDiscoverEvent {
        cwd: temp.path().to_path_buf(),
        reason: SessionReason::Startup,
        scoped_entries: false,
    });
    let _ = handler(&mut evt, &ctx).await.expect("success");

    let cmd_handler = api
        .registered
        .command_context_handlers
        .get("computer")
        .cloned()
        .expect("test value");
    cmd_handler("status", &ctx).await.expect("status command");

    let msgs = notifies.lock().expect("test value");
    assert!(!msgs.is_empty());
    let status_text = &msgs.last().unwrap().0;
    assert!(status_text.contains(&format!("skill: your own computer-use skill is active in place of the built-in guide ({user_skill_path})")));
}

#[tokio::test]
async fn test_given_built_in_skill_contributed_when_status_runs_then_no_skill_line_added() {
    let mut api = make_api();
    let actions = Arc::new(TestSessionActions {
        commands: vec![],
        tools: std::sync::Mutex::new(vec![]),
    });
    api.runtime.bind_session_actions(actions);

    let comp = ComputerUseComponent::new(ComputerUseComponentOptions {
        platform: Some("linux".to_string()),
        load_settings: Some(Arc::new(|_, _| Ok(sample_settings(true, false)))),
        ..Default::default()
    });
    comp.register(&mut api);

    let handler = api
        .registered
        .handlers
        .get(&EventKind::ResourcesDiscover)
        .expect("handler registered")[0]
        .clone();
    let temp = tempfile::tempdir().expect("tempdir");
    let (ctx, notifies) = command_context(temp.path(), ExtensionMode::Tui);

    let mut evt = ExtensionEvent::ResourcesDiscover(ResourcesDiscoverEvent {
        cwd: temp.path().to_path_buf(),
        reason: SessionReason::Startup,
        scoped_entries: false,
    });
    let _ = handler(&mut evt, &ctx).await.expect("success");

    let cmd_handler = api
        .registered
        .command_context_handlers
        .get("computer")
        .cloned()
        .expect("test value");
    cmd_handler("status", &ctx).await.expect("status command");

    let msgs = notifies.lock().expect("test value");
    assert!(!msgs.is_empty());
    let status_text = &msgs.last().unwrap().0;
    let skill_lines: Vec<&str> = status_text
        .lines()
        .filter(|line| line.starts_with("skill:"))
        .collect();
    assert!(skill_lines.is_empty());
}

#[tokio::test]
async fn test_given_concurrent_load_when_shutdown_runs_then_awaits_and_closes() {
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started_tx = Arc::new(tokio::sync::Mutex::new(Some(started_tx)));

    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
    let gate_rx = Arc::new(tokio::sync::Mutex::new(Some(gate_rx)));

    let loader = Arc::new(ComputerRuntimeLoader::new(ComputerRuntimeLoaderOptions {
        host: Arc::new(TestHostApi::default()),
        settings: sample_settings(true, false),
        engine_child: Some(fake_engine_child()),
        on_engine_error: Arc::new(|_| {}),
        on_permission_required: None,
        load_runtime: Some(Arc::new(move || {
            let rx = Arc::clone(&gate_rx);
            let tx = Arc::clone(&started_tx);
            Box::pin(async move {
                if let Some(s) = tx.lock().await.take() {
                    let _ = s.send(());
                }
                let gate = rx.lock().await.take().expect("gate available");
                gate.await.expect("gate signal");
                Ok(maho_omo_computer_use::runtime::create_computer_use_runtime(
                    maho_omo_computer_use::runtime::ComputerUseRuntimeOptions {
                        settings: sample_settings(true, false),
                        execute_tool: None,
                        engine_child: Some(fake_engine_child()),
                        on_engine_error: Arc::new(|_| {}),
                        on_permission_required: None,
                    },
                ))
            })
        })),
    }));

    // Start load in background
    let loader_for_task = Arc::clone(&loader);
    let load_task = tokio::spawn(async move { loader_for_task.load().await });

    // Await bounded oneshot confirming load is running in flight
    started_rx.await.expect("load has started");

    // Check started() returns Some while in-flight
    assert!(loader.is_started());
    let started_fut = loader
        .started()
        .expect("started is Some while load in flight");

    // Release gate
    gate_tx.send(()).expect("send gate");

    // started future completes with loaded runtime
    let rt = started_fut.await.expect("load succeeds");
    assert!(loader.is_started());

    // Clean shutdown closes handle
    let close_res = rt.handle.close().await;
    assert!(close_res.is_ok());

    let task_res = load_task.await.expect("task join");
    assert!(task_res.is_ok());
}

#[tokio::test]
async fn test_given_failed_load_when_started_awaited_then_preserves_error_and_resets_for_retry() {
    let attempt = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started_tx = Arc::new(tokio::sync::Mutex::new(Some(started_tx)));

    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
    let gate_rx = Arc::new(tokio::sync::Mutex::new(Some(gate_rx)));

    let attempt_clone = Arc::clone(&attempt);
    let loader = Arc::new(ComputerRuntimeLoader::new(ComputerRuntimeLoaderOptions {
        host: Arc::new(TestHostApi::default()),
        settings: sample_settings(true, false),
        engine_child: Some(fake_engine_child()),
        on_engine_error: Arc::new(|_| {}),
        on_permission_required: None,
        load_runtime: Some(Arc::new(move || {
            let count = attempt_clone.fetch_add(1, Ordering::SeqCst);
            let s_tx = Arc::clone(&started_tx);
            let g_rx = Arc::clone(&gate_rx);
            Box::pin(async move {
                if count == 0 {
                    if let Some(s) = s_tx.lock().await.take() {
                        let _ = s.send(());
                    }
                    let gate = g_rx.lock().await.take().expect("gate available");
                    gate.await.expect("gate signal");
                    Err("network timeout during runtime download".to_string())
                } else {
                    Ok(maho_omo_computer_use::runtime::create_computer_use_runtime(
                        maho_omo_computer_use::runtime::ComputerUseRuntimeOptions {
                            settings: sample_settings(true, false),
                            execute_tool: None,
                            engine_child: Some(fake_engine_child()),
                            on_engine_error: Arc::new(|_| {}),
                            on_permission_required: None,
                        },
                    ))
                }
            })
        })),
    }));

    // Start first load in background
    let loader_task = Arc::clone(&loader);
    let load_handle = tokio::spawn(async move { loader_task.load().await });

    // Await bounded oneshot confirming load is running in flight
    started_rx.await.expect("first load started");

    // While load is in flight, capture started() future
    let started_fut = loader
        .started()
        .expect("started is Some during in-flight load");

    // Release gate to let load fail
    gate_tx.send(()).expect("send gate");

    let load_res = load_handle.await.expect("task join");
    let started_res = started_fut.await;

    // Both preserve the exact original error
    match load_res {
        Err(e) => assert_eq!(e, "network timeout during runtime download"),
        Ok(_) => panic!("expected load error"),
    }
    match started_res {
        Err(e) => assert_eq!(e, "network timeout during runtime download"),
        Ok(_) => panic!("expected started error"),
    }

    // Failure resets loader state to unstarted (matching upstream loading = undefined)
    assert!(!loader.is_started());
    assert!(loader.started().is_none());

    // Subsequent load retries and succeeds!
    let retry_res = loader.load().await;
    assert!(retry_res.is_ok());
    assert!(loader.is_started());
    assert!(loader.started().is_some());
}
