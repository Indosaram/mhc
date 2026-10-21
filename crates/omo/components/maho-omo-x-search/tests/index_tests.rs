//! Port of omo-senpi `components/x-search/index.test.ts` at latest `455dee62`.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use maho_ext_api::{
    BuildSystemPromptOptions, ComponentLogger, EventBus, EventKind, EventResult, ExecOptions,
    ExecResult, ExecuteToolError, ExecuteToolErrorCode, ExecuteToolFuture, ExecuteToolOptions,
    Extension, ExtensionApi, ExtensionContext, ExtensionFailure, ExtensionFuture, ExtensionMode,
    ExtensionRuntime, ExtensionSessionActions, ExtensionSessionProfile, ExtensionUi,
    ExtensionUiDialogOptions, JsonValue, LazyToolActivator, LoadedExtension, Model, ModelRegistry,
    NotificationType, ResourcesDiscoverEvent, SessionEntry, SessionManager, SessionReason,
    SlashCommandInfo, SourceInfo, SourceOrigin, SourceScope, Theme, ThinkingLevel, ToolExposure,
    ToolSessionManager, UiFuture,
};
use maho_omo_x_search::client::{XSearchFetch, XSearchHttpResponse};
use maho_omo_x_search::index::{
    XSearchComponentOptions, create_x_search_component, resolve_x_search_skill_path,
};
use maho_omo_x_search::tool::X_SEARCH_TOOL_NAME;
use serde_json::{Value, json};

fn probe_fixture() -> Value {
    let content = include_str!("./__fixtures__/x-search-response.probe.json");
    serde_json::from_str(content).expect("parse probe fixture")
}

struct MockModelRegistry {
    stored_cred_type: Option<maho_ai::auth::types::CredentialType>,
    api_key: Option<String>,
}

impl ModelRegistry for MockModelRegistry {
    fn get_all(&self) -> Vec<Model> {
        Vec::new()
    }
    fn get_available(&self) -> Vec<Model> {
        Vec::new()
    }
    fn find(&self, _provider: &str, _id: &str) -> Option<Model> {
        None
    }
    fn has_configured_auth(&self, _model: &Model) -> bool {
        false
    }
    fn get_api_key_for_provider<'a>(
        &'a self,
        _provider: &'a str,
    ) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
    fn get_stored_credential_type(
        &self,
        provider: &str,
    ) -> Result<Option<maho_ai::auth::types::CredentialType>, ExtensionFailure> {
        if provider == "xai" {
            Ok(self.stored_cred_type)
        } else {
            Ok(None)
        }
    }
    fn get_provider_auth<'a>(
        &'a self,
        provider: &'a str,
    ) -> ExtensionFuture<'a, Option<maho_ai::models::AuthResolution>> {
        let key = self.api_key.clone();
        Box::pin(async move {
            if provider == "xai" {
                Ok(key.map(|k| maho_ai::models::AuthResolution {
                    ambient: false,
                    auth: maho_ai::models::ProviderAuthResult {
                        api_key: Some(k),
                        ..Default::default()
                    },
                    env: None,
                }))
            } else {
                Ok(None)
            }
        })
    }
}

struct DummySession;
impl ToolSessionManager for DummySession {
    fn session_id(&self) -> &str {
        "session-1"
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}
impl SessionManager for DummySession {
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

struct DummyUi;
impl ExtensionUi for DummyUi {
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
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(
        &self,
        _: &str,
        _: Option<maho_ext_api::WidgetContent>,
        _: maho_ext_api::ExtensionWidgetOptions,
    ) {
    }
    fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(
        &self,
        _: maho_ext_api::ComponentFactory,
        _: maho_ext_api::CustomUiOptions,
    ) -> ExtensionFuture<'_, Value> {
        Box::pin(async { Ok(Value::Null) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
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
    fn execute_tool<'a>(
        &'a self,
        _: &'a str,
        _: JsonValue,
        _: ExecuteToolOptions,
    ) -> ExecuteToolFuture<'a> {
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
    fn exec<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: &'a Path,
        _: ExecOptions,
    ) -> ExtensionFuture<'a, ExecResult> {
        Box::pin(async { Err(ExtensionFailure::new("unexpected exec")) })
    }
}

fn fake_loaded_extension(name: &str) -> LoadedExtension {
    LoadedExtension::new(
        name,
        PathBuf::from("/test"),
        SourceInfo {
            path: format!("/test/{name}"),
            source: "test".to_string(),
            scope: SourceScope::System,
            origin: SourceOrigin::TopLevel,
            base_dir: Some("/test".to_string()),
        },
    )
}

fn fake_api(name: &str, commands: Vec<SlashCommandInfo>) -> ExtensionApi {
    let runtime = ExtensionRuntime::default();
    runtime.bind_session_actions(Arc::new(FakeSessionActions { commands }));

    ExtensionApi::new(
        fake_loaded_extension(name),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        runtime,
    )
}

#[derive(Default)]
struct RecordingLogger {
    entries: Arc<Mutex<Vec<(String, String)>>>,
}

impl ComponentLogger for RecordingLogger {
    fn info(&self, message: &str, _details: Option<&JsonValue>) {
        self.entries
            .lock()
            .expect("lock entries")
            .push(("info".to_string(), message.to_string()));
    }
    fn warn(&self, message: &str, _details: Option<&JsonValue>) {
        self.entries
            .lock()
            .expect("lock entries")
            .push(("warn".to_string(), message.to_string()));
    }
    fn error(&self, message: &str, _details: Option<&JsonValue>) {
        self.entries
            .lock()
            .expect("lock entries")
            .push(("error".to_string(), message.to_string()));
    }
}

fn write_auth_json(dir: &Path, value: &Value) {
    let mut file = File::create(dir.join("auth.json")).expect("create auth.json");
    let content = serde_json::to_string(value).expect("serialize json");
    file.write_all(content.as_bytes()).expect("write auth.json");
}

fn make_context(
    stored: Option<maho_ai::auth::types::CredentialType>,
    api_key: Option<&str>,
    cwd: PathBuf,
) -> ExtensionContext {
    let registry = Arc::new(MockModelRegistry {
        stored_cred_type: stored,
        api_key: api_key.map(str::to_string),
    });
    ExtensionContext {
        ui: Arc::new(DummyUi),
        mode: ExtensionMode::Print,
        has_ui: false,
        cwd: cwd.clone(),
        agent_dir: cwd,
        session_manager: Arc::new(DummySession),
        model_registry: registry,
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
        logger: None,
        idle_coordinator: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

#[tokio::test]
async fn test_given_host_already_loaded_user_skill_when_discover_then_yields() {
    let agent_dir = tempfile::tempdir().expect("tempdir");
    write_auth_json(
        agent_dir.path(),
        &json!({ "xai": { "type": "oauth", "refresh": "r" } }),
    );

    let home_dir = tempfile::tempdir().expect("tempdir");
    let omo_dir = home_dir.path().join(".omo");
    fs::create_dir_all(&omo_dir).expect("create .omo");
    fs::write(omo_dir.join("omo.jsonc"), "{}").expect("write omo.jsonc");

    let mut env = BTreeMap::new();
    env.insert(
        "HOME".to_string(),
        home_dir.path().to_string_lossy().to_string(),
    );
    env.insert(
        "USERPROFILE".to_string(),
        home_dir.path().to_string_lossy().to_string(),
    );

    let existing_commands = vec![SlashCommandInfo {
        name: "skill:x-search".to_string(),
        description: None,
        argument_hint: None,
        source_info: Some(SourceInfo {
            path: "/home/me/.omo/agent/skills/x-search/SKILL.md".to_string(),
            source: "skill".to_string(),
            scope: SourceScope::System,
            origin: SourceOrigin::TopLevel,
            base_dir: None,
        }),
    }];

    let mut api = fake_api("test", existing_commands);
    let comp = create_x_search_component(XSearchComponentOptions {
        agent_dir: Some(agent_dir.path().to_path_buf()),
        env: Some(env),
        resolve_skill_path: Some(Arc::new(|| {
            Some(PathBuf::from(
                "/plugin/skills-conditional/x-search/SKILL.md",
            ))
        })),
        ..Default::default()
    });
    comp.register(&mut api);

    assert_eq!(api.registered.tools.len(), 1);
    assert_eq!(api.registered.tools[0].definition.name, X_SEARCH_TOOL_NAME);

    let ctx = make_context(None, None, home_dir.path().to_path_buf());
    let mut event = maho_ext_api::ExtensionEvent::ResourcesDiscover(ResourcesDiscoverEvent {
        cwd: home_dir.path().to_path_buf(),
        reason: SessionReason::Startup,
        scoped_entries: true,
    });

    let handlers = api
        .runtime
        .live_handlers("test", EventKind::ResourcesDiscover)
        .expect("handlers registered");
    let event_result = handlers[0](&mut event, &ctx).await.expect("handler run");
    match event_result {
        EventResult::None => {}
        EventResult::ResourcesDiscover(d) => {
            assert!(d.skill_paths.is_empty());
        }
        _ => panic!("unexpected event result"),
    }
}

#[tokio::test]
async fn test_given_disabled_skills_names_x_search_when_discover_then_not_contributed() {
    let agent_dir = tempfile::tempdir().expect("tempdir");
    write_auth_json(
        agent_dir.path(),
        &json!({ "xai": { "type": "oauth", "refresh": "r" } }),
    );

    let home_dir = tempfile::tempdir().expect("tempdir");
    let omo_dir = home_dir.path().join(".omo");
    fs::create_dir_all(&omo_dir).expect("create .omo");
    fs::write(
        omo_dir.join("omo.jsonc"),
        json!({ "disabled_skills": ["x-search"] }).to_string(),
    )
    .expect("write omo.jsonc");

    let mut env = BTreeMap::new();
    env.insert(
        "HOME".to_string(),
        home_dir.path().to_string_lossy().to_string(),
    );
    env.insert(
        "USERPROFILE".to_string(),
        home_dir.path().to_string_lossy().to_string(),
    );

    let mut api = fake_api("test", Vec::new());
    let comp = create_x_search_component(XSearchComponentOptions {
        agent_dir: Some(agent_dir.path().to_path_buf()),
        env: Some(env),
        resolve_skill_path: Some(Arc::new(|| {
            Some(PathBuf::from(
                "/plugin/skills-conditional/x-search/SKILL.md",
            ))
        })),
        ..Default::default()
    });
    comp.register(&mut api);

    assert_eq!(api.registered.tools.len(), 1);

    let ctx = make_context(None, None, home_dir.path().to_path_buf());
    let mut event = maho_ext_api::ExtensionEvent::ResourcesDiscover(ResourcesDiscoverEvent {
        cwd: home_dir.path().to_path_buf(),
        reason: SessionReason::Startup,
        scoped_entries: true,
    });

    let handlers = api
        .runtime
        .live_handlers("test", EventKind::ResourcesDiscover)
        .expect("handlers registered");
    let event_result = handlers[0](&mut event, &ctx).await.expect("handler run");
    match event_result {
        EventResult::None => {}
        EventResult::ResourcesDiscover(d) => {
            assert!(d.skill_paths.is_empty());
        }
        _ => panic!("unexpected event result"),
    }
}

#[tokio::test]
async fn test_given_no_same_name_skill_and_empty_denylist_when_discover_then_contributed() {
    let agent_dir = tempfile::tempdir().expect("tempdir");
    write_auth_json(
        agent_dir.path(),
        &json!({ "xai": { "type": "oauth", "refresh": "r" } }),
    );

    let home_dir = tempfile::tempdir().expect("tempdir");
    let omo_dir = home_dir.path().join(".omo");
    fs::create_dir_all(&omo_dir).expect("create .omo");
    fs::write(omo_dir.join("omo.jsonc"), "{}").expect("write omo.jsonc");

    let mut env = BTreeMap::new();
    env.insert(
        "HOME".to_string(),
        home_dir.path().to_string_lossy().to_string(),
    );
    env.insert(
        "USERPROFILE".to_string(),
        home_dir.path().to_string_lossy().to_string(),
    );

    let existing_commands = vec![SlashCommandInfo {
        name: "skill:frontend".to_string(),
        description: None,
        argument_hint: None,
        source_info: Some(SourceInfo {
            path: "/home/me/.omo/agent/skills/frontend/SKILL.md".to_string(),
            source: "skill".to_string(),
            scope: SourceScope::System,
            origin: SourceOrigin::TopLevel,
            base_dir: None,
        }),
    }];

    let mut api = fake_api("test", existing_commands);
    let comp = create_x_search_component(XSearchComponentOptions {
        agent_dir: Some(agent_dir.path().to_path_buf()),
        env: Some(env),
        resolve_skill_path: Some(Arc::new(|| {
            Some(PathBuf::from(
                "/plugin/skills-conditional/x-search/SKILL.md",
            ))
        })),
        ..Default::default()
    });
    comp.register(&mut api);

    let ctx = make_context(None, None, home_dir.path().to_path_buf());
    let mut event = maho_ext_api::ExtensionEvent::ResourcesDiscover(ResourcesDiscoverEvent {
        cwd: home_dir.path().to_path_buf(),
        reason: SessionReason::Startup,
        scoped_entries: true,
    });

    let handlers = api
        .runtime
        .live_handlers("test", EventKind::ResourcesDiscover)
        .expect("handlers registered");
    let event_result = handlers[0](&mut event, &ctx).await.expect("handler run");
    let EventResult::ResourcesDiscover(d) = event_result else {
        panic!("expected ResourcesDiscover");
    };
    assert_eq!(d.skill_paths.len(), 1);
    assert_eq!(
        d.skill_paths[0].path,
        "/plugin/skills-conditional/x-search/SKILL.md"
    );
}

#[tokio::test]
async fn test_given_stored_xai_oauth_entry_when_registering_then_tool_and_skill_registered() {
    let agent_dir = tempfile::tempdir().expect("tempdir");
    write_auth_json(
        agent_dir.path(),
        &json!({ "xai": { "type": "oauth", "refresh": "r" } }),
    );

    let mut api = fake_api("test", Vec::new());
    let test_env = BTreeMap::from([
        (
            "HOME".to_string(),
            agent_dir.path().to_string_lossy().to_string(),
        ),
        (
            "USERPROFILE".to_string(),
            agent_dir.path().to_string_lossy().to_string(),
        ),
    ]);
    let comp = create_x_search_component(XSearchComponentOptions {
        agent_dir: Some(agent_dir.path().to_path_buf()),
        env: Some(test_env),
        resolve_skill_path: Some(Arc::new(|| {
            Some(PathBuf::from("/test/x-search/skill/SKILL.md"))
        })),
        ..Default::default()
    });
    comp.register(&mut api);

    assert_eq!(api.registered.tools.len(), 1);
    assert_eq!(api.registered.tools[0].definition.name, "x_search");
    assert_eq!(
        api.registered.tools[0].definition.exposure,
        Some(ToolExposure::Search)
    );

    let handlers = api
        .runtime
        .live_handlers("test", EventKind::ResourcesDiscover)
        .expect("handlers registered");
    assert_eq!(handlers.len(), 1);

    let ctx = make_context(None, None, agent_dir.path().to_path_buf());
    let mut event = maho_ext_api::ExtensionEvent::ResourcesDiscover(ResourcesDiscoverEvent {
        cwd: agent_dir.path().to_path_buf(),
        reason: SessionReason::Startup,
        scoped_entries: true,
    });
    let event_result = handlers[0](&mut event, &ctx).await.expect("run");
    let EventResult::ResourcesDiscover(d) = event_result else {
        panic!("expected ResourcesDiscover");
    };
    assert_eq!(d.skill_paths.len(), 1);
    assert!(d.skill_paths[0].path.ends_with("SKILL.md"));
}

#[test]
fn test_given_connected_credential_when_registering_then_startup_stays_quiet_on_debug_only() {
    let agent_dir = tempfile::tempdir().expect("tempdir");
    write_auth_json(agent_dir.path(), &json!({ "xai": { "type": "oauth" } }));

    let skill_dir = tempfile::tempdir().expect("tempdir");
    let skill_file = skill_dir.path().join("SKILL.md");
    fs::write(
        &skill_file,
        "---
name: x-search
---
",
    )
    .expect("write skill");
    let skill_file_clone = skill_file.clone();

    let logger = Arc::new(RecordingLogger::default());
    let mut api = fake_api("test", Vec::new());
    let comp = create_x_search_component(XSearchComponentOptions {
        agent_dir: Some(agent_dir.path().to_path_buf()),
        env: Some(BTreeMap::new()),
        resolve_skill_path: Some(Arc::new(move || Some(skill_file_clone.clone()))),
        logger: Some(logger.clone()),
        ..Default::default()
    });
    comp.register(&mut api);

    assert_eq!(api.registered.tools.len(), 1);
    let entries = logger.entries.lock().expect("lock entries");
    // Registration stays completely quiet (no info, warn, or error entries)
    assert!(entries.is_empty());
}

#[test]
fn test_packaged_plugin_layout_skill_path_resolution() {
    let plugin_root = tempfile::tempdir().expect("tempdir");
    let root = plugin_root
        .path()
        .canonicalize()
        .expect("canonical tempdir");
    let cond_dir = root.join("skills-conditional/x-search");
    fs::create_dir_all(&cond_dir).expect("create dir");
    fs::write(cond_dir.join("SKILL.md"), "---\nname: x-search\n---\n").expect("write");

    let fake_ext_dir = root.join("extensions");
    fs::create_dir_all(&fake_ext_dir).expect("create dir");

    let resolved = resolve_x_search_skill_path(Some(&fake_ext_dir));
    assert_eq!(resolved, Some(cond_dir.join("SKILL.md")));
}

#[test]
fn test_packaged_plugin_layout_without_skills_conditional_returns_none() {
    let plugin_root = tempfile::tempdir().expect("tempdir");
    let fake_ext_dir = plugin_root.path().join("extensions");
    fs::create_dir_all(&fake_ext_dir).expect("create dir");

    let resolved = resolve_x_search_skill_path(Some(&fake_ext_dir));
    assert_eq!(resolved, None);
}

#[test]
fn test_given_connected_credential_but_no_resolvable_skill_warns_and_registers_tool() {
    let agent_dir = tempfile::tempdir().expect("tempdir");
    write_auth_json(agent_dir.path(), &json!({ "xai": { "type": "oauth" } }));

    let logger = Arc::new(RecordingLogger::default());
    let mut api = fake_api("test", Vec::new());
    let comp = create_x_search_component(XSearchComponentOptions {
        agent_dir: Some(agent_dir.path().to_path_buf()),
        env: Some(BTreeMap::new()),
        resolve_skill_path: Some(Arc::new(|| None)),
        logger: Some(logger.clone()),
        ..Default::default()
    });
    comp.register(&mut api);

    assert_eq!(api.registered.tools.len(), 1);
    assert!(
        api.runtime
            .live_handlers("test", EventKind::ResourcesDiscover)
            .is_none()
    );

    let entries = logger.entries.lock().expect("lock entries");
    assert!(
        entries
            .iter()
            .any(|(lvl, msg)| lvl == "warn" && msg.contains("x-search skill missing"))
    );
}

#[test]
fn test_given_no_xai_entry_and_no_xai_api_key_skips_registration() {
    let agent_dir = tempfile::tempdir().expect("tempdir");
    write_auth_json(
        agent_dir.path(),
        &json!({ "anthropic": { "type": "oauth" } }),
    );

    let logger = Arc::new(RecordingLogger::default());
    let mut api = fake_api("test", Vec::new());
    let comp = create_x_search_component(XSearchComponentOptions {
        agent_dir: Some(agent_dir.path().to_path_buf()),
        env: Some(BTreeMap::new()),
        logger: Some(logger.clone()),
        ..Default::default()
    });
    comp.register(&mut api);

    assert_eq!(api.registered.tools.len(), 0);
    assert!(
        api.runtime
            .live_handlers("test", EventKind::ResourcesDiscover)
            .is_none()
    );

    let entries = logger.entries.lock().expect("lock entries");
    // Skipping registration is silent on debug channel, so logger entries are empty
    assert!(entries.is_empty());
}

#[test]
fn test_given_no_stored_entry_but_xai_api_key_then_tool_registers() {
    let agent_dir = tempfile::tempdir().expect("tempdir");
    let mut env = BTreeMap::new();
    env.insert("XAI_API_KEY".to_string(), "env-token".to_string());

    let mut api = fake_api("test", Vec::new());
    let comp = create_x_search_component(XSearchComponentOptions {
        agent_dir: Some(agent_dir.path().to_path_buf()),
        env: Some(env),
        ..Default::default()
    });
    comp.register(&mut api);

    assert_eq!(api.registered.tools.len(), 1);
}

#[test]
fn test_given_malformed_stored_xai_entry_plus_xai_api_key_stored_owns_decision() {
    let agent_dir = tempfile::tempdir().expect("tempdir");
    write_auth_json(
        agent_dir.path(),
        &json!({ "xai": { "type": "totally-unknown" } }),
    );

    let mut env = BTreeMap::new();
    env.insert("XAI_API_KEY".to_string(), "env-token".to_string());

    let mut api = fake_api("test", Vec::new());
    let comp = create_x_search_component(XSearchComponentOptions {
        agent_dir: Some(agent_dir.path().to_path_buf()),
        env: Some(env),
        ..Default::default()
    });
    comp.register(&mut api);

    assert_eq!(api.registered.tools.len(), 0);
    assert!(
        api.runtime
            .live_handlers("test", EventKind::ResourcesDiscover)
            .is_none()
    );
}

#[test]
fn test_given_invalid_auth_json_then_nothing_registers() {
    let agent_dir = tempfile::tempdir().expect("tempdir");
    fs::write(agent_dir.path().join("auth.json"), "{ not json").expect("write");

    let mut env = BTreeMap::new();
    env.insert("XAI_API_KEY".to_string(), "env-token".to_string());

    let mut api = fake_api("test", Vec::new());
    let comp = create_x_search_component(XSearchComponentOptions {
        agent_dir: Some(agent_dir.path().to_path_buf()),
        env: Some(env),
        ..Default::default()
    });
    comp.register(&mut api);

    assert_eq!(api.registered.tools.len(), 0);
}

#[test]
fn test_given_no_agent_dir_option_then_home_resolved_from_env() {
    let agent_dir = tempfile::tempdir().expect("tempdir");
    write_auth_json(agent_dir.path(), &json!({ "xai": { "type": "api_key" } }));

    let mut env = BTreeMap::new();
    env.insert(
        "OMO_CODING_AGENT_DIR".to_string(),
        agent_dir.path().to_string_lossy().to_string(),
    );

    let mut api = fake_api("test", Vec::new());
    let comp = create_x_search_component(XSearchComponentOptions {
        env: Some(env),
        ..Default::default()
    });
    comp.register(&mut api);

    assert_eq!(api.registered.tools.len(), 1);
    assert_eq!(maho_omo_x_search::XSearchComponent::NAME, "x-search");
}

#[tokio::test]
async fn test_registered_tool_executes_successfully() {
    let agent_dir = tempfile::tempdir().expect("tempdir");
    write_auth_json(agent_dir.path(), &json!({ "xai": { "type": "oauth" } }));

    let fetch_impl: XSearchFetch = Arc::new(|_url, _headers, _body| {
        let fixture_body = serde_json::to_string(&probe_fixture()).unwrap();
        Box::pin(async move {
            Ok(XSearchHttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: fixture_body,
            })
        })
    });

    let mut api = fake_api("test", Vec::new());
    let comp = create_x_search_component(XSearchComponentOptions {
        agent_dir: Some(agent_dir.path().to_path_buf()),
        env: Some(BTreeMap::new()),
        fetch_impl: Some(fetch_impl),
        ..Default::default()
    });
    comp.register(&mut api);

    let executor = api
        .runtime
        .extension_tool_executor("/test/test", X_SEARCH_TOOL_NAME)
        .expect("registered tool executor");

    let ctx = make_context(
        Some(maho_ai::auth::types::CredentialType::OAuth),
        Some("stored-token"),
        PathBuf::from("/tmp"),
    );

    let result = executor(
        "call-1",
        json!({ "query": "Grok CLI", "from_date": "2026-09-01" }),
        None,
        None,
        &ctx,
    )
    .await
    .expect("execution succeeds");

    let text = match &result.content[0] {
        maho_ext_api::ContentBlock::Text(t) => t.text.clone(),
        _ => String::new(),
    };
    assert!(text.starts_with("x_search results:"));
}

#[tokio::test]
async fn test_registered_tool_returns_auth_error_when_no_bearer() {
    let agent_dir = tempfile::tempdir().expect("tempdir");
    write_auth_json(agent_dir.path(), &json!({ "xai": { "type": "oauth" } }));

    let fetch_impl: XSearchFetch = Arc::new(|_url, _headers, _body| {
        Box::pin(async move {
            Ok(XSearchHttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: "{}".to_string(),
            })
        })
    });

    let mut api = fake_api("test", Vec::new());
    let comp = create_x_search_component(XSearchComponentOptions {
        agent_dir: Some(agent_dir.path().to_path_buf()),
        env: Some(BTreeMap::new()),
        fetch_impl: Some(fetch_impl),
        ..Default::default()
    });
    comp.register(&mut api);

    let executor = api
        .runtime
        .extension_tool_executor("/test/test", X_SEARCH_TOOL_NAME)
        .expect("registered tool executor");
    let ctx = make_context(None, None, PathBuf::from("/tmp"));

    let result = executor("call-1", json!({ "query": "Grok CLI" }), None, None, &ctx)
        .await
        .expect("execution succeeds");

    let text = match &result.content[0] {
        maho_ext_api::ContentBlock::Text(t) => t.text.clone(),
        _ => String::new(),
    };
    assert!(text.contains("x_search error [AUTH]"));
}

#[tokio::test]
async fn test_registered_tool_handles_caller_abort_through_executor() {
    let agent_dir = tempfile::tempdir().expect("tempdir");
    write_auth_json(agent_dir.path(), &json!({ "xai": { "type": "oauth" } }));

    let controller = maho_ai::utils::abort::AbortController::new();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started_tx = Arc::new(std::sync::Mutex::new(Some(started_tx)));

    let fetch_impl: XSearchFetch = Arc::new(move |_url, _headers, _body| {
        let started_tx = started_tx.clone();
        Box::pin(async move {
            if let Some(tx) = started_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
            std::future::pending::<()>().await;
            Ok(XSearchHttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: "{}".to_string(),
            })
        })
    });

    let mut api = fake_api("test", Vec::new());
    let comp = create_x_search_component(XSearchComponentOptions {
        agent_dir: Some(agent_dir.path().to_path_buf()),
        env: Some(BTreeMap::new()),
        fetch_impl: Some(fetch_impl),
        ..Default::default()
    });
    comp.register(&mut api);

    let executor = api
        .runtime
        .extension_tool_executor("/test/test", X_SEARCH_TOOL_NAME)
        .expect("registered tool executor");

    let ctx = make_context(
        Some(maho_ai::auth::types::CredentialType::OAuth),
        Some("stored-token"),
        PathBuf::from("/tmp"),
    );

    let controller_clone = controller.clone();
    let driver = tokio::spawn(async move {
        started_rx.await.expect("fetch started");
        controller_clone.abort(None);
    });

    let result = executor(
        "call-1",
        json!({ "query": "Grok CLI" }),
        Some(controller.signal()),
        None,
        &ctx,
    )
    .await
    .expect("execution succeeds");

    driver.await.expect("driver join");

    let text = match &result.content[0] {
        maho_ext_api::ContentBlock::Text(t) => t.text.clone(),
        _ => String::new(),
    };
    assert!(text.contains("x_search error [UPSTREAM]"));
    assert!(text.contains("cancelled by the caller"));
}
