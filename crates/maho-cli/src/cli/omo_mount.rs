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
        // Installed skills win; fall back to the library default root and only warn when the
        // resolved directory is absent, so a dev tree still mounts the OMO composition.
        let skills_root = environment.get(maho_omo::SKILLS_ROOT_ENV).map(std::path::PathBuf::from)
            .or_else(|| environment.get("OMO_SENPI_PLUGIN_ROOT").map(|root| Path::new(root).join("skills")))
            .unwrap_or_else(maho_omo::builtin_skills_root);
        if !skills_root.is_dir() { eprintln!("omo mount: packaged skills directory is missing: {}", skills_root.display()); }
        // State dir: the immutable-latest contract is the agent-home product-identity path
        // (`getOmoNativeStateDir` -> `<agentDir>/omo-senpi/omo-native`), which `OmoComponentOptions`
        // already resolves; the onboarding marker/decline/cooldown files are agent-global (they carry
        // per-project hashes INSIDE), so a project-local dir would be a divergence. Do not override it
        // with `cwd`; `..Default::default()` keeps the library's own resolution.
        Ok(Self::shipped(task, memory, &maho_omo::OmoComponentOptions {
            skills_root, env: environment, ..Default::default()
        }, OmoRuntimeOptions {
            logger: Some(Arc::new(maho_omo::logger::SinkLogger::new(Arc::new(|level, message, details| {
                let diagnostic = match details {
                    Some(details) => format!("maho-omo {}: {message} {details}\n", level.as_str()),
                    None => format!("maho-omo {}: {message}\n", level.as_str()),
                };
                maho_core::output_guard::maho_write_stderr(&diagnostic);
            })))),
            ..Default::default()
        }, provisioning))
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

/// Builds ONE retained [`super::memory_runtime::MemoryRuntime`] per mount, before extension
/// loading, over the real host ports. Its ports read the shared OMO runtime lazily (captured
/// tools, the memory disabled flag) and a retained `ExtensionApi` for the reflection completion
/// renderer; both cells are filled when the memory component registers inside the OMO composition.
/// The runtime is retained by the component closure for the whole mount, across reloads.
fn memory_for_parent(parent: super::default_extensions::TaskParent, cwd: &Path, agent_dir: &Path, env: std::collections::BTreeMap<String, String>) -> Result<OmoSenpiComponent, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    // The supervisor is an installed entrypoint; fall back to the current binary when the
    // packaging has not published one, so the mount still assembles in a dev tree.
    let supervisor = env.get("MAHO_MEMORY_SUPERVISOR").map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_file())
        .unwrap_or_else(|| executable.clone());
    let omo_cell: Arc<Mutex<Option<OmoRuntime>>> = Arc::new(Mutex::new(None));
    let api_cell: Arc<Mutex<Option<ExtensionApi>>> = Arc::new(Mutex::new(None));

    let config_cwd = cwd.to_path_buf(); let config_env = env.clone();
    let load_config: super::memory_runtime::LiveMemoryConfig = Arc::new(move || Ok(super::task_runners::resolved_omo_config(&config_cwd, &config_env)));
    // Keep a handle for the resident model resolver: `load_config` itself is MOVED into the host below.
    let resident_config = Arc::clone(&load_config);
    let sources_agent = agent_dir.to_path_buf(); let sources_cwd = cwd.to_path_buf();
    let config_sources = Arc::new(move || {
        let mut paths = vec![sources_agent.join("models.json"), sources_agent.join("settings.json"), sources_agent.join("settings.jsonc")];
        for ancestor in sources_cwd.ancestors() { paths.extend([ancestor.join(".omo/omo.json"), ancestor.join(".omo/omo.jsonc")]); }
        paths.into_iter().map(|path| maho_omo_memory::worker::model_preflight::ConfigSource { exists: path.is_file(), path }).collect()
    });
    let captured_cell = omo_cell.clone();
    let captured_tools = Arc::new(move || captured_cell.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
        .map(|runtime| runtime.captured_tools().into_iter().map(|tool| tool.name).collect()).unwrap_or_default());
    let disabled_cell = omo_cell.clone();
    let disabled = Arc::new(move || disabled_cell.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
        .is_some_and(|runtime| runtime.context().get_flag(&maho_omo::component_disabled_flag("memory")) == Some(maho_ext_api::FlagValue::Boolean(true))));
    let renderer_cell = api_cell.clone();
    let ensure_completion_renderer = Arc::new(move || {
        if let Some(api) = renderer_cell.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
            maho_omo_memory::worker::completion_renderers::register_reflection_completion_renderer(api,
                Arc::new(|theme: &maho_ext_api::Theme| Arc::new(MemoryTheme(theme.clone())) as Arc<dyn maho_omo_memory::worker::entry_renderers::EntryRenderTheme>));
        }
    });
    let paths = env.clone();
    let which = Arc::new(move |name: &str| paths.get("PATH").into_iter().flat_map(|path| std::env::split_paths(path))
        .map(|path| path.join(name)).find(|path| path.is_file()).map(|path| path.to_string_lossy().into_owned()));
    let warn: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |message: &str| eprintln!("memory: {message}"));
    let recall_warn: Arc<dyn Fn(&str) + Send + Sync> = Arc::clone(&warn);

    let runtime = super::memory_runtime::MemoryRuntime::new(super::memory_runtime::MemoryRuntimeHost {
        cwd: cwd.to_path_buf(), agent_dir: agent_dir.to_path_buf(), env: env.clone(),
        load_config, config_sources,
        launcher: maho_omo_memory::worker::model_preflight::Launcher { command: executable.to_string_lossy().into_owned(), prefix_args: Vec::new() },
        supervisor_command: supervisor, supervisor_args: vec!["--memory-supervisor".to_owned()],
        actions: Arc::new(super::default_extensions::TaskActions(parent.clone())),
        ensure_completion_renderer, captured_tools, disabled,
        parent_cache_reusable: Arc::new(|context| context.session_manager.session_file().is_some() && context.model.as_ref().is_some_and(|model| model.api == "anthropic-messages")),
        which, warn,
    })?;

    let recall_env = env.clone();
    let recall_actions: Arc<dyn maho_ext_api::ExtensionActions> = Arc::new(super::default_extensions::TaskActions(parent.clone()));
    let recall_persona = memory_core::recall::kibitzer_persona().to_string();
    // `runtime.settings()` is the MEMORY subtree; `resolve_agent_recall_settings` returns the
    // RECALL BLOCK itself. The BASE block drives the sidecar's event caps; the per-agent block is
    // resolved PER SPAWN inside the factory, so a settings change reaches the next child.
    let memory_settings = runtime.settings().unwrap_or(serde_json::Value::Null);
    let base_recall = maho_omo_memory::reflection_settings::resolve_agent_recall_settings(Some(&memory_settings), "")
        .unwrap_or(serde_json::Value::Null);
    let event_caps = super::kibitzer_tools::resolve_kibitzer_event_caps(&base_recall);
    let recall_ports = maho_omo_memory::kibitzer_delivery::mount_recall_ports(maho_omo_memory::kibitzer_delivery::MountRecallPortsInput {
        actions: Arc::clone(&recall_actions),
        executor: tokio::runtime::Handle::current(),
        resolve_context: runtime.resolve_context(),
        env: recall_env.clone(),
        warn: Arc::clone(&recall_warn),
    });
    // The producer-owned per-session registry: the host retrieves the SAME `KibitzerSessionResources`
    // the sidecar uses, and passes the same accessor into the wiring so both sides share it.
    let session_resources_for = maho_omo_memory::kibitzer_session_resources::KibitzerSessionResourceRegistry::new().getter();
    let resources: super::kibitzer_child::KibitzerChildResourcesFactory = {
        let runtime = runtime.clone();
        let cwd = cwd.to_string_lossy().into_owned();
        let persona = recall_persona.clone();
        let recall_env = recall_env.clone();
        let session_resources_for = Arc::clone(&session_resources_for);
        Arc::new(move |input: &maho_omo_memory::kibitzer_contract::KibitzerChildSpawnInput| {
            let session_id = input.session_id.clone();
            // The SAME resources object the sidecar uses for this session: one budget slot, one nudge
            // binding, shared by all five tools and the runner.
            let session_resources = session_resources_for(&session_id);
            // Bind this child's `memory.recall.max_items` on the SHARED handle BEFORE any tool is built;
            // the member tools and the nudge wrapper read the live bound value per call.
            session_resources.bind_max_items(input.max_items);
            // Dynamic per-AGENT caps, resolved at SPAWN so a settings change reaches the next child.
            let agent_id = runtime.resolve_context()(&session_id).map(|identity| identity.identity).unwrap_or_default();
            let settings = runtime.settings().unwrap_or(serde_json::Value::Null);
            let recall = maho_omo_memory::reflection_settings::resolve_agent_recall_settings(Some(&settings), &agent_id)
                .unwrap_or(serde_json::Value::Null);
            let tool_caps = super::kibitzer_tools::resolve_kibitzer_tool_caps(&recall);
            // RAW entries for THIS explicit session; the PRODUCER owns the cursor, so neither resolver
            // pre-skips (the `since` argument is passed through, never applied here).
            let this = runtime.clone();
            let entries_for = session_id.clone();
            let session_entries: super::kibitzer_tools::SessionEntriesResolver = Arc::new(move || this.session_entries_for(&entries_for));
            let member_entries: Arc<dyn Fn(i64) -> Vec<serde_json::Value> + Send + Sync> = {
                let this = runtime.clone();
                let entries_for = session_id.clone();
                Arc::new(move |_since: i64| this.session_entries_for(&entries_for))
            };
            // ALL five tools charge the SAME producer-owned slot the sidecar resets per wake.
            let budget_slot = Arc::clone(&session_resources.budget_slot);
            let budget: super::kibitzer_tools::KibitzerBudgetSource = Arc::new(move || budget_slot.current());
            let mut tools = super::kibitzer_tools::create_kibitzer_host_tools(super::kibitzer_tools::KibitzerHostToolsInput {
                cwd: cwd.clone(), session_entries, caps: tool_caps, budget,
            });
            tools.extend(maho_omo_memory::kibitzer_member_tools::kibitzer_member_tools(maho_omo_memory::kibitzer_member_tools::KibitzerMemberToolsInput {
                session_id,
                resolve_context: runtime.resolve_context(),
                env: recall_env.clone(),
                session_entries: member_entries,
                resources: session_resources,
                caps: tool_caps,
                // `memory.recall.query_expansion` from the SAME per-agent recall block used for caps.
                query_expansion: recall.get("query_expansion").and_then(serde_json::Value::as_bool).unwrap_or(false),
            }));
            super::kibitzer_child::KibitzerChildResources { persona: persona.clone(), tools }
        })
    };
    let recall_wiring = super::recall_wiring::create_memory_recall_wiring(super::recall_wiring::CliRecallWiringPorts {
        executor: tokio::runtime::Handle::current(),
        cwd: cwd.to_string_lossy().into_owned(),
        agent_dir: agent_dir.to_string_lossy().into_owned(),
        // The resident model resolver's inputs: the FULL config, the pinned category, and the
        // per-session registry snapshot from the SAME retained contexts the member tools read.
        config: resident_config,
        category: base_recall.get("category").and_then(serde_json::Value::as_str).map(str::to_owned),
        registry_for: { let this = runtime.clone(); Arc::new(move |session_id: &str| this.session_registry_for(session_id)) },
        resources,
        session_resources_for,
        actions: Arc::clone(&recall_actions),
        resolve_context: runtime.resolve_context(),
        resolve_settings: { let this = runtime.clone(); Arc::new(move || this.settings()) },
        env: Arc::new(move |key: &str| recall_env.get(key).cloned()),
        warn: Arc::clone(&recall_warn),
        caps: event_caps,
        task_summary: None,
        tool_budget: base_recall.get("tool_budget").and_then(serde_json::Value::as_u64).map(|value| value as usize),
        max_concurrent_wakes: base_recall.get("max_concurrent_wakes").and_then(serde_json::Value::as_u64).map(|value| value as usize),
        sidecar_max_tokens: base_recall.get("sidecar_max_tokens").and_then(serde_json::Value::as_i64),
        send_message: recall_ports.send_message,
        pending_for: recall_ports.pending_for,
        coordinator: Some(super::kibitzer_coordinator::create_kibitzer_coordinator(omo_cell.clone())),
        drain_pending_for: recall_ports.drain_pending_for,
        drain_queued: recall_ports.drain_queued,
    });

    Ok(OmoSenpiComponent::from_context_register("memory", move |api, omo_runtime| {
        *omo_cell.lock().unwrap_or_else(PoisonError::into_inner) = Some(omo_runtime.clone());
        *api_cell.lock().unwrap_or_else(PoisonError::into_inner) = Some(ExtensionApi::new(api.registered.clone(), api.profile.clone(), api.events.clone(), api.runtime.clone()));
        maho_omo_memory::composition::MemoryExtension::new(runtime.options()).register(api);
        let recall_theme: maho_omo_memory::worker::completion_renderers::ResolveEntryTheme = Arc::new(|theme: &maho_ext_api::Theme| Arc::new(MemoryTheme(theme.clone())) as Arc<dyn maho_omo_memory::worker::entry_renderers::EntryRenderTheme>);
        recall_wiring.register(api, recall_theme);
    }))
}

struct MemoryTheme(maho_ext_api::Theme);
impl maho_omo_memory::worker::entry_renderers::EntryRenderTheme for MemoryTheme {
    fn fg(&self, tone: &str, text: &str) -> String {
        let color = self.0.colors.get(tone).map(String::as_str).unwrap_or("");
        let prefix = color.strip_prefix('#').filter(|hex| hex.len() == 6).and_then(|hex| u32::from_str_radix(hex, 16).ok())
            .map(|rgb| format!("\x1b[38;2;{};{};{}m", (rgb >> 16) & 255, (rgb >> 8) & 255, rgb & 255)).unwrap_or_else(|| "\x1b[39m".into());
        format!("{prefix}{text}\x1b[39m")
    }
    fn italic(&self, text: &str) -> String { format!("\x1b[3m{text}\x1b[23m") }
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
