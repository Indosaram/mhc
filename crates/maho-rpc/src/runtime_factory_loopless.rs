//! Port of senpi `packages/coding-agent/src/modes/rpc/runtime-factory-loopless.ts`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use maho_agent::execution::plane::NestedRequestFn;
use maho_core::agent_session_services::{
    AgentSessionRuntimeDiagnostic, AgentSessionRuntimeDiagnosticType,
    CreateAgentSessionServicesOptions, create_agent_session_services,
};
use maho_core::config::home_dir;
use maho_core::execution::hand_wake_route::{late_loop_route, loop_wake_route};
use maho_core::resource_loader::{ResourceLoaderInputs, load_project_context_files};
use maho_core::runtime::bootstrap::{ToolDefinitionEntry, ToolRegistryBuild};
use maho_core::settings_manager::SettingsManager;
use maho_core::skills::{LoadSkillsOptions, Skill, load_skills};
use maho_core::system_prompt::ContextFile;
use maho_core::tool_dispatch::tool_dispatcher::{
    ToolDispatcherOptions, ToolRuntimeKind, create_tool_dispatcher, dispatch_tool_call,
};
use maho_ext_api::{
    BeforeAgentStartCombinedResult, BeforeAgentStartEvent, BuildSystemPromptOptions,
    ComponentFactory, CustomComponentFactory, CustomUiFactoryOptions, CustomUiOptions, EventKind,
    ExtensionContext, ExtensionEvent, ExtensionFailure, ExtensionFuture, ExtensionMode,
    ExtensionSessionProfile, ExtensionUi, ExtensionUiDialogOptions, ExtensionUiFactories,
    ExtensionWidgetOptions, FlagValue, FooterComponentFactory, InputEvent, InputEventResult, Model,
    ModelRegistry as ExtModelRegistry, NotificationType, RegisteredTool, SessionEntry,
    SessionManager as ExtSessionManager, SourceInfo, Theme, ToolCallEvent, ToolCallEventResult,
    ToolDefinition, ToolResultEvent, ToolResultEventResult, ToolSessionManager,
    TuiComponentFactory, UiFuture, WidgetContent,
};
use maho_tools::filesystem_policy::FilesystemPolicyChecker;
use maho_tools::index::{ToolsOptions, create_all_tool_definitions};
use maho_tools::tool_definition_wrapper::{
    ContextFactory, ToolRunDispatcher, wrap_tool_definition,
};
use serde_json::{Map, Value};
use tokio::sync::Mutex;

use crate::hand_file_ops::HandFileGuards;
use crate::hand_tool_catalog::{HandCatalogResources, HandToolCatalogSource, with_tool_catalog};
use crate::loop_hook_call::create_loop_hook_call;
use crate::loopless_brain_calls::create_brain_calls;
use crate::loopless_execute_grant::{
    LooplessExecuteGrant, LooplessExecuteRefusedError, is_loopless_execute_granted,
    loopless_grant_runs_tool_handlers,
};
use crate::loopless_loop_hooks::extension_loop_hooks;
use crate::loopless_runtime::{LooplessRuntime, LooplessRuntimeFactory, LooplessToolNotFoundError};
use crate::loopless_runtime_settings::{
    LooplessCliConfiguration, LooplessToolFilters, loopless_tool_selection,
    resolve_loopless_settings,
};
use crate::loopless_tool_call::{LooplessToolCallRequest, run_loopless_tool_call};
use crate::loopless_tool_handlers::{
    LooplessExtensionErrorListener, LooplessExtensionRunner, TwoSidedHome, extension_tool_handlers,
};

/// Extension load failure reported by a [`LooplessExtensionLoader`].
#[derive(Debug, Clone)]
pub struct LooplessExtensionLoadError {
    pub extension_path: String,
    pub error: String,
}

/// Loaded extensions and diagnostic outputs produced by [`LooplessExtensionLoader::load`].
pub struct LoadedLooplessExtensions {
    pub runner: Arc<Mutex<dyn LooplessExtensionRunner>>,
    pub errors: Vec<LooplessExtensionLoadError>,
    pub flag_diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
}

/// Inputs passed to [`LooplessExtensionLoader::load`].
pub struct LooplessExtensionLoadRequest<'a> {
    pub cwd: &'a Path,
    pub base_context: ExtensionContext,
    pub session_profile: ExtensionSessionProfile,
    pub no_extensions: bool,
    pub flag_values: &'a BTreeMap<String, FlagValue>,
}

/// Adapter trait isolating `maho_ext_host` extension loading (`load_extensions_async`,
/// `NativeAsyncExtensionFactory`, `ExtensionRunner`) from `maho-rpc`'s dependency graph.
/// Downstream `maho-cli` implements this using `maho_ext_host`.
#[async_trait]
pub trait LooplessExtensionLoader: Send + Sync {
    async fn load(&self, req: LooplessExtensionLoadRequest<'_>) -> LoadedLooplessExtensions;
}

struct EmptyExtensionRunner;

#[async_trait]
impl LooplessExtensionRunner for EmptyExtensionRunner {
    fn has_handlers(&self, _kind: EventKind) -> bool {
        false
    }

    async fn emit_tool_call(
        &mut self,
        _event: &mut ToolCallEvent,
    ) -> Result<Option<ToolCallEventResult>, ExtensionFailure> {
        Ok(None)
    }

    async fn emit_tool_result(
        &mut self,
        _event: ToolResultEvent,
    ) -> Result<Option<ToolResultEventResult>, ExtensionFailure> {
        Ok(None)
    }

    async fn emit_input(
        &mut self,
        _input: InputEvent,
    ) -> Result<InputEventResult, ExtensionFailure> {
        Ok(InputEventResult::Continue)
    }

    async fn emit_before_agent_start(
        &mut self,
        _event: BeforeAgentStartEvent,
    ) -> Result<Option<BeforeAgentStartCombinedResult>, ExtensionFailure> {
        Ok(None)
    }

    async fn emit_before_provider_request_with_metadata(
        &mut self,
        payload: Value,
        _model: Option<Model>,
        _headers: Option<BTreeMap<String, Option<String>>>,
        _exclude_path: Option<&str>,
    ) -> Result<Value, ExtensionFailure> {
        Ok(payload)
    }

    async fn emit(&mut self, _event: ExtensionEvent) -> Result<(), ExtensionFailure> {
        Ok(())
    }

    fn on_error(&mut self, _listener: LooplessExtensionErrorListener) {}

    fn invalidate(&self, _message: &str) {}

    fn get_all_registered_tools(&self) -> Vec<RegisteredTool> {
        Vec::new()
    }

    fn filesystem_policy(&self) -> Option<FilesystemPolicyChecker> {
        None
    }
}

struct EmptyExtensionLoader;

#[async_trait]
impl LooplessExtensionLoader for EmptyExtensionLoader {
    async fn load(&self, _req: LooplessExtensionLoadRequest<'_>) -> LoadedLooplessExtensions {
        LoadedLooplessExtensions {
            runner: Arc::new(Mutex::new(EmptyExtensionRunner)),
            errors: Vec::new(),
            flag_diagnostics: Vec::new(),
        }
    }
}

#[derive(Clone, Default)]
pub struct LooplessRuntimeFactoryOptions {
    pub extension_loader: Option<Arc<dyn LooplessExtensionLoader>>,
    pub startup_settings_manager: Option<Arc<SettingsManager>>,
    /// Required for `execute`; see `loopless_execute_grant`.
    pub execute_grant: Option<LooplessExecuteGrant>,
    /// Where two-sided extensions run whole; see `extension_tool_handlers`. Default: `Loop`.
    pub two_sided_home: TwoSidedHome,
}

struct SessionlessSessionManager;
impl ToolSessionManager for SessionlessSessionManager {
    fn session_id(&self) -> &str {
        ""
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}
impl ExtSessionManager for SessionlessSessionManager {
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

struct SessionlessToolContext {
    cwd: PathBuf,
    session: SessionlessSessionManager,
}

impl maho_tools::definition::ToolContext for SessionlessToolContext {
    fn cwd(&self) -> &Path {
        &self.cwd
    }
    fn model(&self) -> Option<&Model> {
        None
    }
    fn thinking_level(&self) -> Option<maho_ai::types::ThinkingLevel> {
        None
    }
    fn session_manager(&self) -> &dyn ToolSessionManager {
        &self.session
    }
    fn goal_store_file(&self) -> Option<&Path> {
        None
    }
}

struct SessionlessModelRegistry;
impl ExtModelRegistry for SessionlessModelRegistry {
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

struct NoOpUi;
impl ExtensionUi for NoOpUi {
    fn factories(&self) -> Option<&dyn ExtensionUiFactories> {
        Some(self)
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
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, Value> {
        Box::pin(async { Err(maho_ext_api::ExtensionFailure::new("UI not available")) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}
impl ExtensionUiFactories for NoOpUi {
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
    ) -> ExtensionFuture<'_, Value> {
        Box::pin(async { Err(maho_ext_api::ExtensionFailure::new("UI not available")) })
    }
}

fn sessionless_context(cwd: &str, agent_dir: &str) -> ExtensionContext {
    let cwd_path = PathBuf::from(cwd);
    ExtensionContext {
        ui: Arc::new(NoOpUi),
        mode: ExtensionMode::Rpc,
        has_ui: false,
        cwd: cwd_path.clone(),
        agent_dir: PathBuf::from(agent_dir),
        session_manager: Arc::new(SessionlessSessionManager),
        model_registry: Arc::new(SessionlessModelRegistry),
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
        get_system_prompt_options_fn: Arc::new(move || BuildSystemPromptOptions {
            cwd: cwd_path.clone(),
            ..Default::default()
        }),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        idle_coordinator: None,
        logger: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

fn build_tool_registry(
    base_tools: BTreeMap<String, ToolDefinition>,
    runner: &dyn LooplessExtensionRunner,
    context_factory: ContextFactory,
    dispatcher_fn: ToolRunDispatcher,
    filters: &LooplessToolFilters,
) -> ToolRegistryBuild {
    let mut build = ToolRegistryBuild::default();
    let excluded = filters.excluded_tool_names.as_ref();
    let allowed = filters.allowed_tool_names.as_ref();

    let admits = |name: &str, is_builtin: bool| -> bool {
        if excluded.is_some_and(|set| set.contains(name)) {
            return false;
        }
        if let Some(allowed) = allowed {
            return allowed.contains(name);
        }
        if is_builtin
            && let Some(defaults) = filters.default_tool_names.as_ref()
            && !defaults.contains(name)
        {
            return false;
        }
        true
    };

    for (name, def) in base_tools {
        if !admits(&name, true) {
            continue;
        }
        let agent_tool = wrap_tool_definition(
            def.clone(),
            Some(context_factory.clone()),
            Some(dispatcher_fn.clone()),
        );
        build.definitions.insert(
            name.clone(),
            ToolDefinitionEntry {
                definition: def,
                source_info: SourceInfo::default(),
            },
        );
        build.tool_registry.insert(name, agent_tool);
    }

    for registered in runner.get_all_registered_tools() {
        let name = registered.definition.name.clone();
        if !admits(&name, false) {
            continue;
        }
        let agent_tool = wrap_tool_definition(
            registered.definition.clone(),
            Some(context_factory.clone()),
            Some(dispatcher_fn.clone()),
        );
        build.extension_tools.push(registered.definition.clone());
        build.definitions.insert(
            name.clone(),
            ToolDefinitionEntry {
                definition: registered.definition,
                source_info: registered.source_info,
            },
        );
        build.tool_registry.insert(name, agent_tool);
    }

    build
}

struct LooplessCatalogResources {
    context_files: Vec<ContextFile>,
    skills: Vec<Skill>,
}

impl HandCatalogResources for LooplessCatalogResources {
    fn context_files(&self) -> Vec<ContextFile> {
        self.context_files.clone()
    }
    fn skills(&self) -> Vec<Skill> {
        self.skills.clone()
    }
}

struct RunnerFileGuards {
    runner: Arc<Mutex<dyn LooplessExtensionRunner>>,
    handlers: Arc<dyn crate::loopless_tool_call::LooplessToolHandlers>,
}

impl HandFileGuards for RunnerFileGuards {
    fn before_tool_call<'a>(
        &'a self,
        tool_name: &'a str,
        tool_call_id: &'a str,
        input: &'a mut Map<String, Value>,
        nested: Option<NestedRequestFn>,
    ) -> maho_ai::types::BoxFuture<'a, Result<Option<ToolCallEventResult>, String>> {
        Box::pin(async move {
            let mut val = Value::Object(input.clone());
            let res = self
                .handlers
                .before_tool_call(crate::loopless_tool_call::BeforeToolCallArgs {
                    tool_name,
                    tool_call_id,
                    input: &mut val,
                    nested,
                })
                .await?;
            if let Value::Object(updated) = val {
                *input = updated;
            }
            Ok(res)
        })
    }

    fn filesystem_policy(&self) -> Option<FilesystemPolicyChecker> {
        self.runner
            .try_lock()
            .ok()
            .and_then(|runner| runner.filesystem_policy())
    }
}

/// Builds loopless runtimes from the same launch configuration as `createCliRuntimeFactory`:
/// cwd-bound services (settings, project trust, model registry, extensions and their flags), then
/// the tool registry on a runner without a session, so nothing is ever written to a session file.
pub fn create_loopless_runtime_factory(
    configuration: LooplessCliConfiguration,
    local: LooplessRuntimeFactoryOptions,
) -> LooplessRuntimeFactory {
    Arc::new(move |profile| {
        let configuration = configuration.clone();
        let local = local.clone();
        Box::pin(async move {
            let startup_settings = match &local.startup_settings_manager {
                Some(s) => s,
                None => &SettingsManager::create(
                    &configuration.cwd,
                    &configuration.agent_dir,
                    &home_dir(),
                    true,
                ),
            };
            let mut diagnostics: Vec<AgentSessionRuntimeDiagnostic> = Vec::new();
            let resolved_settings = resolve_loopless_settings(
                &configuration,
                startup_settings,
                &profile,
                &mut diagnostics,
            )
            .await;
            let tool_selection =
                loopless_tool_selection(&configuration, &resolved_settings.settings_manager);
            let resource_inputs = ResourceLoaderInputs {
                additional_extension_paths: configuration.extensions.clone(),
                additional_skill_paths: configuration.skills.clone(),
                additional_prompt_template_paths: configuration.prompt_templates.clone(),
                no_skills: configuration.no_skills,
                no_prompt_templates: configuration.no_prompt_templates,
                ..Default::default()
            };
            let services = create_agent_session_services(CreateAgentSessionServicesOptions {
                cwd: profile.cwd.clone(),
                agent_dir: Some(configuration.agent_dir.clone()),
                settings_manager: Some(resolved_settings.settings_manager),
                model_runtime: None,
            })
            .await
            .map_err(|e| e.to_string())?;
            diagnostics.extend(services.diagnostics.iter().cloned());

            let session_profile = ExtensionSessionProfile::default();
            let base_context = sessionless_context(&services.cwd, &services.agent_dir);
            let default_loader: Arc<dyn LooplessExtensionLoader> = Arc::new(EmptyExtensionLoader);
            let loader = local.extension_loader.as_ref().unwrap_or(&default_loader);
            let loaded_extensions = loader
                .load(LooplessExtensionLoadRequest {
                    cwd: Path::new(&services.cwd),
                    base_context,
                    session_profile,
                    no_extensions: configuration.no_extensions,
                    flag_values: &resolved_settings.flag_values,
                })
                .await;
            diagnostics.extend(loaded_extensions.flag_diagnostics);
            for err in &loaded_extensions.errors {
                diagnostics.push(AgentSessionRuntimeDiagnostic {
                    kind: AgentSessionRuntimeDiagnosticType::Warning,
                    message: format!(
                        "Failed to load extension \"{}\": {}",
                        err.extension_path, err.error
                    ),
                });
            }

            let tool_runtime: ToolRuntimeKind =
                configuration.tool_runtime.unwrap_or_default().into();
            let dispatcher = create_tool_dispatcher(ToolDispatcherOptions::new(tool_runtime));

            let runner = loaded_extensions.runner;

            let runner_for_policy = Arc::clone(&runner);
            let policy_checker: FilesystemPolicyChecker = Arc::new(move |req| {
                let runner = Arc::clone(&runner_for_policy);
                Box::pin(async move {
                    let checker = {
                        let guard = runner.lock().await;
                        guard.filesystem_policy()
                    };
                    match checker {
                        Some(check) => check(req).await,
                        None => Ok(maho_tools::filesystem_policy::FilesystemPolicyDecision::Allow),
                    }
                })
            });

            let mut tools_opts = ToolsOptions::default();
            tools_opts.read.filesystem_policy = Some(policy_checker.clone());
            tools_opts.write.filesystem_policy = Some(policy_checker.clone());
            tools_opts.edit.filesystem_policy = Some(policy_checker.clone());
            tools_opts.grep.filesystem_policy = Some(policy_checker.clone());
            tools_opts.find.filesystem_policy = Some(policy_checker.clone());
            tools_opts.glob.filesystem_policy = Some(policy_checker.clone());
            tools_opts.ls.filesystem_policy = Some(policy_checker);

            let base_tools = create_all_tool_definitions(Path::new(&services.cwd), tools_opts);
            let tool_cwd = PathBuf::from(&services.cwd);
            let context_factory: ContextFactory = Arc::new(move |_, _| {
                Arc::new(SessionlessToolContext {
                    cwd: tool_cwd.clone(),
                    session: SessionlessSessionManager,
                })
            });
            let dispatcher_for_wrap = Arc::clone(&dispatcher);
            let tool_run_dispatcher: ToolRunDispatcher = Arc::new(move |def, call| {
                dispatch_tool_call(Arc::clone(&dispatcher_for_wrap), def, call)
            });

            let registry = {
                let runner_guard = runner.lock().await;
                Arc::new(build_tool_registry(
                    base_tools,
                    &*runner_guard,
                    context_factory,
                    tool_run_dispatcher,
                    &tool_selection.filters,
                ))
            };

            let brain_calls = Arc::new(create_brain_calls());
            let late_route = late_loop_route(profile.wake.clone(), Arc::new(|_| {}));
            let _ = profile
                .wake
                .as_ref()
                .map(|w| loop_wake_route(Arc::clone(w), None));
            let loop_hook_call = Arc::new(create_loop_hook_call(late_route));

            let tool_handlers = loopless_grant_runs_tool_handlers(local.execute_grant.as_ref())
                .then(|| extension_tool_handlers(Arc::clone(&runner), local.two_sided_home));

            let grant = local.execute_grant.clone();
            let registry_for_exec = Arc::clone(&registry);
            let brain_for_exec = Arc::clone(&brain_calls);
            let handlers_for_exec = tool_handlers.clone();
            let execute: crate::loopless_runtime::LooplessExecuteFn = Arc::new(
                move |name, tool_call_id, params, signal, nested, facts| {
                    let grant = grant.clone();
                    let registry = Arc::clone(&registry_for_exec);
                    let brain_calls = Arc::clone(&brain_for_exec);
                    let handlers = handlers_for_exec.clone();
                    Box::pin(async move {
                        if !is_loopless_execute_granted(grant.as_ref()) {
                            return Err(LooplessExecuteRefusedError::new(
                                "no executor runs the tool_call/tool_result handlers for this runtime yet",
                            )
                            .to_string());
                        }
                        let Some(tool) = registry.tool_registry.get(&name).cloned() else {
                            return Err(LooplessToolNotFoundError::new(name).to_string());
                        };
                        let run_key = tool_call_id.clone();
                        Ok(brain_calls
                            .run(&run_key, facts, async move {
                                run_loopless_tool_call(
                                    &tool,
                                    LooplessToolCallRequest {
                                        tool_call_id,
                                        params,
                                        signal,
                                        nested,
                                    },
                                    handlers.as_ref(),
                                )
                                .await
                            })
                            .await)
                    })
                },
            );

            let file_guards: Option<Arc<dyn HandFileGuards>> = match &tool_handlers {
                Some(handlers) if is_loopless_execute_granted(local.execute_grant.as_ref()) => {
                    Some(Arc::new(RunnerFileGuards {
                        runner: Arc::clone(&runner),
                        handlers: Arc::clone(handlers),
                    }))
                }
                _ => None,
            };

            let context_files = if configuration.no_context_files {
                Vec::new()
            } else {
                load_project_context_files(&services.cwd, &services.agent_dir, None).files
            };
            let skills_res = load_skills(&LoadSkillsOptions {
                cwd: services.cwd.clone(),
                agent_dir: services.agent_dir.clone(),
                skill_paths: resource_inputs.additional_skill_paths,
                include_defaults: !resource_inputs.no_skills,
            });
            let catalog_resources = Arc::new(LooplessCatalogResources {
                context_files,
                skills: skills_res.skills,
            });

            let run_loop_hooks = with_tool_catalog(
                extension_loop_hooks(
                    Arc::clone(&runner),
                    local.two_sided_home,
                    services.cwd.clone(),
                    loop_hook_call,
                ),
                Arc::new(HandToolCatalogSource {
                    registry: Arc::clone(&registry),
                    initial_active_tool_names: tool_selection.initial_active_tool_names,
                    allowed_tool_names: tool_selection.filters.allowed_tool_names,
                    resources: catalog_resources,
                }),
            );

            let runner_for_dispose = Arc::clone(&runner);
            let dispatcher_for_dispose = Arc::clone(&dispatcher);
            let dispose: crate::loopless_runtime::LooplessDisposeFn = Arc::new(move || {
                let runner = Arc::clone(&runner_for_dispose);
                let dispatcher = Arc::clone(&dispatcher_for_dispose);
                Box::pin(async move {
                    runner
                        .lock()
                        .await
                        .invalidate("This loopless runtime was disposed.");
                    dispatcher.dispose().await;
                    Ok(())
                })
            });

            Ok(LooplessRuntime {
                cwd: services.cwd,
                definitions: registry.definitions.clone(),
                diagnostics,
                execute,
                run_loop_hooks,
                file_guards,
                dispose,
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loopless_execute_grant::grant_loopless_execute_to_test_harness;
    use crate::loopless_runtime::LooplessRuntimeProfile;
    use serde_json::json;

    #[tokio::test]
    async fn builds_loopless_runtime_refuses_without_grant_and_runs_with_grant() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agent_dir = dir.path().join("agent");
        std::fs::create_dir_all(&agent_dir).expect("agent_dir");
        let cfg = LooplessCliConfiguration {
            cwd: dir.path().to_string_lossy().into_owned(),
            agent_dir: agent_dir.to_string_lossy().into_owned(),
            no_extensions: true,
            no_skills: true,
            ..Default::default()
        };

        let ungranted_factory =
            create_loopless_runtime_factory(cfg.clone(), LooplessRuntimeFactoryOptions::default());
        let ungranted = ungranted_factory(LooplessRuntimeProfile::new(&cfg.cwd))
            .await
            .expect("build");
        let refused = ungranted
            .execute("read", "1", json!({"path": "x"}), None, None, None)
            .await
            .expect_err("ungranted");
        assert!(refused.contains("loopless_execute_refused"));
        ungranted.dispose().await.expect("dispose");

        let grant = grant_loopless_execute_to_test_harness().expect("grant");
        let granted_factory = create_loopless_runtime_factory(
            cfg.clone(),
            LooplessRuntimeFactoryOptions {
                execute_grant: Some(grant),
                ..Default::default()
            },
        );
        let granted = granted_factory(LooplessRuntimeProfile::new(&cfg.cwd))
            .await
            .expect("build granted");
        assert!(granted.definitions.contains_key("read"));
        let catalog = granted
            .run_loop_hooks("describe_tools", json!({"sessionId": "s", "idle": true}))
            .await
            .expect("describe_tools");
        assert!(catalog["tools"].as_array().is_some_and(|a| !a.is_empty()));
        granted.dispose().await.expect("dispose");
    }
}
