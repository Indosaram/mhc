//! Port of the host half of senpi `omo-senpi/src/index.ts`.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use maho_core::agent_session::AgentSession;
use maho_ext_api::{Extension, ExtensionApi, ExtensionSessionProfile, ExtensionUi};
use maho_ext_host::loader::{load_extensions, LoadExtensionsResult, NativeExtensionFactory};
use maho_ext_host::ExtensionRunner;
use maho_omo::{
    OMO_EXTENSION_IDENTITY, OmoComponentOptions, OmoExtension, OmoRuntime, OmoRuntimeOptions,
    OmoSenpiComponent, ProvisioningOptions, memory_component_from, omo_senpi_extension,
};

pub struct OmoMount {
    extension: Arc<OmoExtension>,
}

impl OmoMount {
    pub fn for_parent(parent: super::default_extensions::TaskParent, cwd: &Path, agent_dir: &Path, env: std::collections::BTreeMap<String, String>) -> Result<Self, String> {
        let provisioning = maho_omo::provisioning::ProvisioningOptions {
            toolkit_base_dir: env.get("OMO_AGENT_TOOLKIT_BIN").and_then(|bin| Path::new(bin).parent().map(Path::to_path_buf))
                .or_else(|| env.get("OMO_SENPI_PLUGIN_ROOT").map(|root| Path::new(root).join("runtime/agent-toolkit"))),
            dag_sdk_base_dir: env.get(maho_omo::DAG_SDK_ROOT_ENV).map(Into::into)
                .or_else(|| env.get("OMO_SENPI_PLUGIN_ROOT").map(|root| Path::new(root).join("runtime/dag"))),
            env: Some(Arc::new({ let env = env.clone(); move |name| env.get(name).cloned() })),
            sink: None,
        };
        let mut environment = env.clone();
        for (name, value) in maho_omo::provisioning::provision_toolkit_path(&provisioning).into_iter()
            .chain(maho_omo::provisioning::provision_dag_sdk_root(&provisioning)) {
            match value { Some(value) => { environment.insert(name, value); }, None => { environment.remove(&name); } }
        }
        let task = super::default_extensions::task_entry(parent.clone(), environment.clone());
        let memory = memory_for_parent(parent, cwd, agent_dir, environment.clone())?;
        let skills_root = environment.get(maho_omo::SKILLS_ROOT_ENV).map(std::path::PathBuf::from)
            .or_else(|| environment.get("OMO_SENPI_PLUGIN_ROOT").map(|root| Path::new(root).join("skills")))
            .ok_or("Packaged skills require OMO_SENPI_SKILLS_ROOT or OMO_SENPI_PLUGIN_ROOT")?;
        if !skills_root.is_dir() { return Err(format!("Packaged skills directory is missing: {}", skills_root.display())); }
        Ok(Self::shipped(task, memory, &maho_omo::OmoComponentOptions {
            skills_root, state_dir: cwd.join(".maho"), env: environment,
        }, Default::default(), provisioning))
    }
    pub fn shipped(
        task: OmoSenpiComponent,
        memory: OmoSenpiComponent,
        components: &OmoComponentOptions,
        runtime: OmoRuntimeOptions,
        provisioning: ProvisioningOptions,
    ) -> Self {
        Self { extension: Arc::new(omo_senpi_extension(task, memory, components, runtime, provisioning)) }
    }

    pub fn extension(&self) -> &Arc<OmoExtension> {
        &self.extension
    }

    pub fn runtime(&self) -> Option<Arc<OmoRuntime>> {
        self.extension.runtime()
    }

    pub fn factory(&self) -> NativeExtensionFactory {
        let path = format!("<builtin:{OMO_EXTENSION_IDENTITY}>");
        NativeExtensionFactory {
            path: path.clone(),
            source_info: maho_ext_api::SourceInfo { path, source: "builtin".to_owned(), ..Default::default() },
            extension: Box::new(SharedOmoExtension(Arc::clone(&self.extension))),
        }
    }
}

fn memory_for_parent(parent: super::default_extensions::TaskParent, cwd: &Path, agent_dir: &Path, env: std::collections::BTreeMap<String, String>) -> Result<OmoSenpiComponent, String> {
    let supervisor = env.get("MAHO_MEMORY_SUPERVISOR").map(std::path::PathBuf::from)
        .ok_or("Native memory supervisor package must supply MAHO_MEMORY_SUPERVISOR")?;
    if !supervisor.is_absolute() || !supervisor.is_file() { return Err("MAHO_MEMORY_SUPERVISOR must name an installed absolute executable".into()); }
    let cwd = cwd.to_path_buf(); let agent_dir = agent_dir.to_path_buf();
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let state = Arc::new(Mutex::new(None::<Arc<super::memory_runtime::MemoryRuntime>>));
    Ok(OmoSenpiComponent::from_context_register("memory", move |api, runtime| {
        use maho_ext_api::Extension;
        let registered = Arc::new(Mutex::new(maho_ext_api::ExtensionApi::new(api.registered.clone(), api.profile.clone(), api.events.clone(), api.runtime.clone())));
        let memory = {
            let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.is_none() {
                let config_cwd = cwd.clone(); let config_env = env.clone();
                let config = Arc::new(move || Ok(super::task_runners::resolved_omo_config(&config_cwd, &config_env)));
                let tools = runtime.context().get_captured_tools.clone();
                let flags = runtime.context().config.clone();
                let paths = env.clone();
                let logger = runtime.logger();
                let sources = agent_dir.clone(); let source_cwd = cwd.clone();
                let theme = runtime.logger();
                let built = super::memory_runtime::MemoryRuntime::new(super::memory_runtime::MemoryRuntimeHost {
                    cwd: cwd.clone(), agent_dir: agent_dir.clone(), env: env.clone(), load_config: config,
                    config_sources: Arc::new(move || {
                        let mut paths = vec![sources.join("models.json"), sources.join("settings.json"), sources.join("settings.jsonc")];
                        for ancestor in source_cwd.ancestors() { paths.extend([ancestor.join(".omo/omo.json"), ancestor.join(".omo/omo.jsonc")]); }
                        paths.into_iter().map(|path| maho_omo_memory::worker::model_preflight::ConfigSource { exists: path.is_file(), path }).collect()
                    }),
                    launcher: maho_omo_memory::worker::model_preflight::Launcher { command: executable.to_string_lossy().into_owned(), prefix_args: Vec::new() },
                    supervisor_command: supervisor.clone(), supervisor_args: Vec::new(),
                    actions: Arc::new(super::default_extensions::TaskActions(parent.clone())),
                    ensure_completion_renderer: Arc::new({ let registered = registered.clone(); move || {
                        maho_omo_memory::worker::completion_renderers::register_reflection_completion_renderer(
                            &mut registered.lock().unwrap_or_else(PoisonError::into_inner), Arc::new(|theme| Arc::new(MemoryTheme(theme.clone()))));
                        theme.info("memory reflection completion renderer registered", None);
                    } }),
                    captured_tools: Arc::new(move || tools().into_iter().map(|tool| tool.name).collect()),
                    disabled: Arc::new(move || flags("omo-senpi-memory-disabled") == Some(maho_ext_api::FlagValue::Boolean(true))),
                    parent_cache_reusable: Arc::new(|context| context.session_manager.session_file().is_some() && context.model.as_ref().is_some_and(|model| model.api == "anthropic-messages")),
                    which: Arc::new(move |name| paths.get("PATH").into_iter().flat_map(|path| std::env::split_paths(path)).map(|path| path.join(name)).find(|path| path.is_file()).map(|path| path.to_string_lossy().into_owned())),
                    warn: Arc::new(move |message| logger.warn(message, None)),
                });
                match built { Ok(memory) => *state = Some(memory), Err(error) => panic!("Memory host assembly failed: {error}") }
            }
            state.as_ref().cloned()
        };
        let Some(memory) = memory else { return; };
        maho_omo_memory::composition::MemoryExtension::new(memory.options()).register(api);
        for tool in &mut api.registered.tools {
            let execute = tool.definition.execute.clone(); let memory = memory.clone();
            tool.definition.execute = Arc::new(move |call| { memory.capture_context(call.context); execute(call) });
        }
        let handlers = std::mem::take(&mut api.registered.handlers);
        for (kind, handlers) in handlers {
            for original in handlers {
                let memory = memory.clone();
                api.on(kind, Arc::new(move |event, context| {
                    memory.capture_context(context); original(event, context)
                }));
            }
        }
    }))
}

struct MemoryTheme(maho_ext_api::Theme);
impl maho_omo_memory::worker::entry_renderers::EntryRenderTheme for MemoryTheme {
    fn fg(&self, tone: &str, text: &str) -> String { self.0.fg(tone, text) }
    fn italic(&self, text: &str) -> String { self.0.italic(text) }
}

pub fn memory_entry(build: impl Fn() -> maho_omo_memory::composition::MemoryExtensionOptions + Send + Sync + 'static) -> OmoSenpiComponent {
    memory_component_from(build)
}

pub fn task_component(
    engine: maho_omo_task::engine::TaskEngine,
    spawn: senpi_task::tools::task::execute_spec::TaskToolDeps,
    ownership: senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps,
) -> OmoSenpiComponent {
    let state = Arc::new(Mutex::new((engine, spawn, ownership)));
    OmoSenpiComponent::from_register("task", move |api| {
        let state = state.lock().unwrap_or_else(PoisonError::into_inner);
        let (engine, spawn, ownership) = &*state;
        let engine = retained_engine(engine);
        let spawn = spawn.clone();
        let ownership = senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps {
            state_dir: ownership.state_dir.clone(), team_bounds: ownership.team_bounds,
            load_runtime_state: ownership.load_runtime_state.clone(),
        };
        if let Err(error) = maho_omo_task::component::TaskComponent::register(api, engine, spawn, ownership, false) {
            eprintln!("omo task component registration failed: {}", error.message);
        }
    })
}

pub fn retained_engine(engine: &maho_omo_task::engine::TaskEngine) -> maho_omo_task::engine::TaskEngine {
    maho_omo_task::engine::TaskEngine {
        manager: engine.manager.clone(), lifecycle: engine.lifecycle.clone(),
        notifier: engine.notifier.clone(), runtime: engine.runtime.clone(),
        store: engine.store.clone(), config: engine.config.clone(), agents: engine.agents.clone(),
    }
}

struct SharedOmoExtension(Arc<OmoExtension>);

impl Extension for SharedOmoExtension {
    fn register(&self, api: &mut ExtensionApi) {
        self.0.register(api);
    }
}

pub fn bind_runtime_into_context(runtime: &OmoRuntime, context: &mut maho_ext_api::ExtensionContext) {
    runtime.bind(context);
}

/// senpi's noninteractive `noOpUIContext`; dialogs have no answer and widgets have no surface.
pub struct NoninteractiveUi;

impl ExtensionUi for NoninteractiveUi {
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: maho_ext_api::ExtensionUiDialogOptions) -> maho_ext_api::UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: maho_ext_api::ExtensionUiDialogOptions) -> maho_ext_api::UiFuture<'a, bool> { Box::pin(async { false }) }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: maho_ext_api::ExtensionUiDialogOptions) -> maho_ext_api::UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn notify(&self, _: &str, _: maho_ext_api::NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<maho_ext_api::WidgetContent>, _: maho_ext_api::ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String { String::new() }
    fn custom(&self, _: maho_ext_api::ComponentFactory, _: maho_ext_api::CustomUiOptions) -> maho_ext_api::ExtensionFuture<'_, serde_json::Value> { Box::pin(async { Ok(serde_json::Value::Null) }) }
    fn theme(&self) -> maho_ext_api::Theme { Default::default() }
}

pub async fn mount_base_extensions(
    session: &AgentSession,
    ui: Arc<dyn ExtensionUi>,
    base: Vec<NativeExtensionFactory>,
) -> Result<LoadExtensionsResult, String> {
    let cwd = session.cwd();
    let loaded = load_extensions(base, Path::new(&cwd), ExtensionSessionProfile::default());
    bind_loaded_extensions(session, ui, &loaded, None).await?;
    Ok(loaded)
}

pub async fn bind_loaded_extensions(
    session: &AgentSession,
    ui: Arc<dyn ExtensionUi>,
    loaded: &LoadExtensionsResult,
    omo: Option<&OmoMount>,
) -> Result<(), String> {
    let mut context = session.extension_context(ui);
    if let Some(runtime) = omo.and_then(OmoMount::runtime) {
        bind_runtime_into_context(&runtime, &mut context);
    }
    let runner = ExtensionRunner::new(loaded.extensions.clone(), loaded.runtime.clone(), loaded.events.clone(), context);
    session.set_extension_runner(runner).await;
    Ok(())
}

pub async fn mount_native_extensions_with_omo(
    session: &AgentSession,
    ui: Arc<dyn ExtensionUi>,
    base: Vec<NativeExtensionFactory>,
    omo: &OmoMount,
) -> Result<LoadExtensionsResult, String> {
    let mut factories = base;
    factories.push(omo.factory());
    let cwd = session.cwd();
    let loaded = load_extensions(factories, Path::new(&cwd), ExtensionSessionProfile::default());
    let mut context = session.extension_context(ui);
    if let Some(runtime) = omo.runtime() {
        bind_runtime_into_context(&runtime, &mut context);
    }
    let runner = ExtensionRunner::new(loaded.extensions.clone(), loaded.runtime.clone(), loaded.events.clone(), context);
    session.set_extension_runner(runner).await;
    Ok(loaded)
}
