//! Production host assembly for pinned omo-senpi memory `wiring.ts`.
//!
//! The mount owner constructs ONE `MemoryRuntime` per retained OMO mount, then
//! uses `options()` in its memory factory. `capture_context` must run before
//! each event/tool dispatch (including resumed RPC contexts). Configuration is
//! a live, resolved OMO root object, never a `{global, project}` snapshot.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
};

use maho_ext_api::{ExtensionActions, ExtensionContext, ExtensionUi, NotificationType};
use maho_omo_memory::commands::people_ask::PeopleAskLauncher;
use maho_omo_memory::commands::register::{
    DreamCommandOutcome, DreamRequestSink, ManualDreamCommandRequest, MemoryCommandDeps,
};
use maho_omo_memory::{
    composition::MemoryExtensionOptions,
    context::MemoryIdentityContext,
    facts_runner::NativeFactsAttemptOptions,
    index::{MemoryComponent, MemoryComponentOptions},
    wiring::{MemoryWiring, create_memory_wiring},
    wiring_runtime::MemoryRuntimeWiring,
    wiring_static::MemoryStaticOptions,
    wiring_types::{MemoryWiringOptions, NativeMemoryWiringOptions},
    worker,
};
use memory_core::{
    git::{GitMemoryRepo, exec::system_git_exec},
    reflection::{ReflectionReservationStore, ReservedRun},
};
use serde_json::Value;

pub type LiveMemoryConfig = Arc<dyn Fn() -> Result<Value, String> + Send + Sync>;
pub type MemoryWarning = Arc<dyn Fn(&str) + Send + Sync>;
/// The host's `which` resolver (pinned `WhichFn`): a command name to its resolved path.
pub type WhichResolver = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Required production dependencies; launcher and supervisor are installed
/// native entrypoints, not fixture processes or guessed executable-relative paths.
pub struct MemoryRuntimeHost {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub env: BTreeMap<String, String>,
    pub load_config: LiveMemoryConfig,
    pub config_sources: Arc<dyn Fn() -> Vec<worker::model_preflight::ConfigSource> + Send + Sync>,
    pub launcher: worker::model_preflight::Launcher,
    pub supervisor_command: PathBuf,
    pub supervisor_args: Vec<String>,
    pub actions: Arc<dyn ExtensionActions>,
    pub ensure_completion_renderer: Arc<dyn Fn() + Send + Sync>,
    pub captured_tools: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    pub disabled: Arc<dyn Fn() -> bool + Send + Sync>,
    pub parent_cache_reusable: Arc<dyn Fn(&ExtensionContext) -> bool + Send + Sync>,
    pub which: WhichResolver,
    pub warn: MemoryWarning,
}

struct IdentityWorker {
    identity: MemoryIdentityContext,
    store: Arc<ReflectionReservationStore>,
    runner: tokio::sync::Mutex<worker::runner::SenpiSubprocessRunner>,
    cache: tokio::sync::Mutex<worker::model_preflight::ModelPreflight>,
    sandbox: Mutex<maho_omo_memory::identity_runtime::IdentitySandbox>,
}

/// Retain this alongside the mount across extension reloads. Every resolver,
/// extractor, trigger and worker shares this component and this wiring object.
pub struct MemoryRuntime {
    pub component: Arc<MemoryComponent>,
    pub wiring: Arc<tokio::sync::Mutex<MemoryWiring>>,
    host: MemoryRuntimeHost,
    current: Mutex<Option<ExtensionContext>>,
    /// Retained contexts bind member tools to an explicit session and are pruned on shutdown.
    contexts: Mutex<BTreeMap<String, ExtensionContext>>,
    workers: Mutex<BTreeMap<String, Arc<IdentityWorker>>>,
    /// Per-bind write latch (pin `wiring-memory-write.ts:20-38` reads `SessionState`, rebuilt at
    /// `session_start`); entries are dropped on `session_start` and `session_shutdown`.
    write_sessions: Mutex<
        BTreeMap<String, Arc<Mutex<maho_omo_memory::wiring_memory_write::MemoryWriteSession>>>,
    >,
    nudge: Arc<Mutex<maho_omo_memory::nudge_wiring::MemoryNudgeWiring>>,
    skills: maho_omo_memory::skills_usage_wiring::SkillsUsageTrackers,
    health_notices: Mutex<BTreeSet<String>>,
    prompt: Arc<maho_omo_memory::prompt::MemoryPromptHandler>,
    static_options: Mutex<Option<MemoryStaticOptions>>,
}

impl MemoryRuntime {
    fn register_host(self: &Arc<Self>, api: &mut maho_ext_api::ExtensionApi) {
        maho_omo_memory::bindings::register_memory_binding_renderer(api);
        let this = self.clone();
        for kind in [
            maho_ext_api::EventKind::BeforeAgentStart,
            maho_ext_api::EventKind::SessionStart,
            maho_ext_api::EventKind::AgentSettled,
            maho_ext_api::EventKind::ToolCall,
        ] {
            let this = this.clone();
            api.on(
                kind,
                Arc::new(move |_, context| {
                    this.capture_context(context);
                    Box::pin(async { Ok(maho_ext_api::EventResult::None) })
                }),
            );
        }
        let this = self.clone();
        api.on(
            maho_ext_api::EventKind::SessionStart,
            Arc::new(move |_, context| {
                this.write_sessions
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(context.session_manager.session_id());
                Box::pin(async { Ok(maho_ext_api::EventResult::None) })
            }),
        );
        let this = self.clone();
        api.on(
            maho_ext_api::EventKind::SessionShutdown,
            Arc::new(move |_, context| {
                this.contexts
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(context.session_manager.session_id());
                this.write_sessions
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(context.session_manager.session_id());
                Box::pin(async { Ok(maho_ext_api::EventResult::None) })
            }),
        );
        let this = self.clone();
        let resolve_session = Arc::new(move |context: &ExtensionContext| {
            this.capture_context(context);
            this.identity(context.session_manager.session_id())
                .map(|identity| {
                    (
                        context.session_manager.session_id().into(),
                        identity.identity,
                    )
                })
        });
        let this = self.clone();
        let resolve_active_session = Arc::new(move || {
            this.context()
                .map(|context| context.session_manager.session_id().to_owned())
        });
        let this = self.clone();
        let resolve_settings = Arc::new(move |identity: &str| {
            this.settings()
                .and_then(|settings| {
                    maho_omo_memory::dream_trigger_gates::resolve_dream_trigger_settings(
                        &settings,
                        Some(identity),
                    )
                })
                .unwrap_or_else(|error| {
                    (this.host.warn)(&error);
                    maho_omo_memory::dream_trigger_gates::DreamTriggerSettings {
                        enabled: false,
                        ..Default::default()
                    }
                })
        });
        let this = self.clone();
        let launch: maho_omo_memory::dream_trigger::DreamLaunch =
            Arc::new(move |session, origin, request, signal| {
                let this = this.clone();
                Box::pin(async move {
                    let identity = this.identity(&session).ok_or("no bound memory session")?;
                    let worker = this.worker(&identity)?;
                    let settings = this.settings()?;
                    let policy =
                        maho_omo_memory::dream_trigger_gates::resolve_dream_trigger_settings(
                            &settings,
                            Some(&identity.identity),
                        )?;
                    let runtime = maho_omo_memory::identity_runtime::MemoryIdentityRuntime {
                        identity,
                        store: worker.store.clone(),
                        runner: Default::default(),
                        sandbox: Default::default(),
                    };
                    let mut launch = |run| this.launch(worker.clone(), run, false);
                    let aborted = || {
                        signal
                            .as_ref()
                            .is_some_and(maho_ext_api::AbortSignal::is_aborted)
                    };
                    let mut session = maho_omo_memory::wiring_runtime::RuntimeDreamSession {
                        session_id: session,
                        runtime: &runtime,
                        launch: &mut launch,
                        aborted: &aborted,
                    };
                    maho_omo_memory::dream_trigger_fire::fire_dream(
                        &mut session,
                        origin,
                        &policy,
                        &request,
                        &|| memory_core::support::time::now_millis() as f64,
                        &aborted,
                        &mut |error| {
                            (this.host.warn)(&format!("memory dream launch failed: {error:?}"))
                        },
                    )
                    .map_err(|error| format!("{error:?}"))
                })
            });
        let dream = Arc::new(maho_omo_memory::dream_trigger::DreamTriggerWiring::new(
            maho_omo_memory::dream_trigger::DreamTriggerOptions {
                resolve_session,
                resolve_active_session,
                resolve_settings,
                launch,
                warn: self.host.warn.clone(),
            },
        ));
        dream.register(api);
        // Registration runs before async lifecycle dispatch; this wiring lock
        // cannot be held yet. The evaluator must survive subsequent shutdowns.
        match self.wiring.try_lock() {
            Ok(mut wiring) => wiring.register_shutdown_evaluator(dream.shutdown_evaluator()),
            Err(error) => std::panic::panic_any(format!(
                "memory wiring locked during static registration: {error}"
            )),
        }
        let this = self.clone();
        // Task-10 replaced the ad-hoc deps with commands::types::MemoryCommandDeps; the CLI supplies
        // the live ports and wraps the retained dream wiring in the command-layer sink.
        let dream_sink: Arc<dyn DreamRequestSink> = Arc::new(DreamSink(dream.clone()));
        let command_deps = MemoryCommandDeps {
            resolve_context: {
                let this = this.clone();
                Arc::new(move |session| this.identity(session))
            },
            resolve_identity: Some({
                let this = this.clone();
                Arc::new(move || {
                    this.context()
                        .and_then(|context| this.identity(context.session_manager.session_id()))
                })
            }),
            settings: {
                let this = this.clone();
                Arc::new(move || this.settings())
            },
            bust_prompt_cache: {
                let prompt = self.prompt.clone();
                Arc::new(move || {
                    use maho_omo_memory::projection_pin::ProjectionPins;
                    prompt.cache.clear();
                    prompt.pins.request_refresh();
                })
            },
            config_path: Some({
                let cwd = self.host.cwd.clone();
                let agent_dir = self.host.agent_dir.clone();
                Arc::new(move || {
                    cwd.ancestors()
                        .flat_map(|ancestor| {
                            [
                                ancestor.join(".omo/omo.json"),
                                ancestor.join(".omo/omo.jsonc"),
                            ]
                        })
                        .find(|path| path.is_file())
                        .or_else(|| {
                            [
                                agent_dir.join("settings.json"),
                                agent_dir.join("settings.jsonc"),
                            ]
                            .into_iter()
                            .find(|path| path.is_file())
                        })
                        .map(|path| path.to_string_lossy().into_owned())
                })
            }),
            full_config: {
                let this = this.clone();
                Some(Arc::new(move || (this.host.load_config)()))
            },
            actions: self.host.actions.clone(),
            prompt: self.prompt.clone(),
            sessions_dir: self.host.agent_dir.join("sessions"),
            reflect: Some({
                let this = this.clone();
                Arc::new(move |session, event| {
                    let this = this.clone();
                    Box::pin(async move {
                        let identity = this.identity(&session).ok_or("no bound memory session")?;
                        let worker = this.worker(&identity)?;
                        let result = worker
                            .store
                            .evaluate(&session, event)
                            .map_err(|error| error.to_string())?
                            .ok_or("reflection reservation rejected")?;
                        if result.status == "active" {
                            this.launch(worker.clone(), result.run.clone(), true)?;
                        }
                        let mut recovered_run_id = None;
                        let mut stuck = None;
                        if result.status == "pending" {
                            let runtime = this.clone();
                            let recovery_worker = worker.clone();
                            let reflection = identity.identity_paths.reflection.clone();
                            let run_id = result.run.run_id.clone();
                            let receipt = tokio::task::spawn_blocking(move || {
                                maho_omo_memory::wiring_static::recover_pending_reflection(
                                    &reflection,
                                    run_id,
                                    &|| {
                                        tokio::runtime::Builder::new_current_thread()
                                            .enable_all()
                                            .build()
                                            .map_err(|error| error.to_string())?
                                            .block_on(runtime.reconcile_worker(&recovery_worker))
                                    },
                                )
                            })
                            .await
                            .map_err(|error| error.to_string())?;
                            recovered_run_id = receipt.recovered_run_id;
                            stuck = receipt.stuck;
                        }
                        Ok(maho_omo_memory::commands::types::ReflectionRequestReceipt {
                            disposition: if result.status == "active" {
                                maho_omo_memory::commands::types::ReflectionDisposition::Reserved
                            } else {
                                maho_omo_memory::commands::types::ReflectionDisposition::Pending
                            },
                            run_id: result.run.run_id,
                            recovered_run_id,
                            stuck,
                        })
                    })
                })
            }),
            dream: Some(dream_sink),
            facts_retry: Some({
                let this = this.clone();
                Arc::new(move |identity| {
                    let this = this.clone();
                    Box::pin(async move {
                        let mut wiring = this.wiring.lock().await;
                        let facts = wiring
                            .runtime
                            .existing_facts_wiring(&identity)
                            .ok_or("facts extractor is not bound")?;
                        facts.reconcile_extractor();
                        Ok(())
                    })
                })
            }),
            exec: Some(system_git_exec()),
            env: Some(self.child_env()),
            // Upstream leaves `peopleAsk` unset in production: `/people --ask` builds its runner per
            // call from the live config and the invoking context's model registry.
            people_ask: None,
            launcher: Some(PeopleAskLauncher {
                command: self.host.launcher.command.clone(),
                prefix_args: self.host.launcher.prefix_args.clone(),
            }),
            now: Some(Arc::new(memory_core::support::time::now_millis)),
            is_process_alive: None,
        };
        // Palace registration mirrors upstream `registerMemoryStatic`, which calls
        // `registerPalaceCommand(pi, resolver, resolvePalacePeople)`: the resolver binds the active
        // session identity, and the people resolver reads the resolved `memory.people` gate.
        let this = self.clone();
        let resolve_palace: maho_omo_memory::palace::command::PalaceExtensionResolver =
            Arc::new(move |context: &ExtensionContext| {
                this.capture_context(context);
                this.identity(context.session_manager.session_id())
            });
        let this = self.clone();
        let resolve_palace_people: maho_omo_memory::palace::command::PalacePeopleResolver =
            Arc::new(move || this.palace_people_options());
        maho_omo_memory::palace::command::register_palace_command(
            api,
            resolve_palace,
            Some(resolve_palace_people),
        );
        maho_omo_memory::commands::register::register_memory_commands(api, Arc::new(command_deps));
    }

    pub fn new(host: MemoryRuntimeHost) -> Result<Arc<Self>, String> {
        let mut host = host;
        if !host
            .env
            .contains_key(memory_core::identity::layout::MEMORY_ROOT_ENV_VAR)
            && let Some(home) = host.env.get("HOME")
        {
            host.env.insert(
                memory_core::identity::layout::MEMORY_ROOT_ENV_VAR.to_owned(),
                Path::new(home)
                    .join(".omo/memory")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        let initial = (host.load_config)()?;
        if initial.get("global").is_some() || initial.get("project").is_some() {
            return Err(
                "memory builder requires live resolved OMO config, not a global/project envelope"
                    .into(),
            );
        }
        maho_omo_memory::reflection_settings::resolve_memory_settings(initial.get("memory"))?;
        let component = MemoryComponent::new(MemoryComponentOptions {
            cwd: host.cwd.clone(),
            env: host.env.clone(),
            load_config: host.load_config.clone(),
            now: Arc::new(|| memory_core::support::time::now_millis() as f64),
            disabled: host.disabled.clone(),
            sync_runner: None,
        });
        let skills = Arc::new(Mutex::new(BTreeMap::new()));
        let wiring = Arc::new(tokio::sync::Mutex::new(create_memory_wiring(
            MemoryWiringOptions {
                runtime: MemoryRuntimeWiring::default(),
                skills_usage: skills.clone(),
            },
        )));
        Ok(Arc::new(Self {
            component,
            wiring,
            host,
            skills,
            current: Mutex::new(None),
            contexts: Mutex::new(BTreeMap::new()),
            workers: Mutex::new(BTreeMap::new()),
            write_sessions: Mutex::new(BTreeMap::new()),
            nudge: Arc::new(Mutex::new(Default::default())),
            health_notices: Mutex::new(BTreeSet::new()),
            prompt: Arc::new(maho_omo_memory::prompt::MemoryPromptHandler::persistent()),
            static_options: Mutex::new(None),
        }))
    }

    pub fn capture_context(&self, context: &ExtensionContext) {
        self.contexts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                context.session_manager.session_id().to_owned(),
                context.clone(),
            );
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = Some(context.clone());
    }

    /// The host environment with the `process.env` writes extensions made after it was captured.
    fn child_env(&self) -> BTreeMap<String, String> {
        let mut env = self.host.env.clone();
        maho_ext_api::process_env::apply(&mut env);
        env
    }

    pub fn settings(&self) -> Result<Value, String> {
        let config = (self.host.load_config)()?;
        maho_omo_memory::reflection_settings::resolve_memory_settings(config.get("memory"))
    }

    /// The prompt-context resolver the recall wiring and prompt injection share: a session id to its
    /// bound memory identity, or `None` when the session has no enabled memory binding.
    pub fn resolve_context(self: &Arc<Self>) -> maho_omo_memory::prompt::PromptContextResolver {
        let this = self.clone();
        Arc::new(move |session: &str| this.identity(session))
    }

    /// The bound parent session's entries after a cursor, for the member-scoped `session_entries`
    /// tool. `since` is the count of entries the caller has already seen.
    pub fn session_entries_since(&self, since: i64) -> Vec<Value> {
        let Some(context) = self.context() else {
            return Vec::new();
        };
        context
            .session_manager
            .get_entries()
            .into_iter()
            .skip(since.max(0) as usize)
            .map(|entry| entry.data)
            .collect()
    }

    /// Returns the bound parent's raw entries; the member tool applies its cursor and caps.
    pub fn session_entries_for(&self, session_id: &str) -> Vec<Value> {
        self.contexts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(session_id)
            .map(|context| {
                context
                    .session_manager
                    .get_entries()
                    .into_iter()
                    .map(|entry| entry.data)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The bound session's model-registry snapshot, from the SAME retained `ExtensionContext` the
    /// member tools read (captured on the hook pipeline, pruned on shutdown). `None` means no context
    /// was captured for this session yet - the resolver reports that as `RegistrySnapshotUnavailable`.
    pub fn session_registry_for(
        &self,
        session_id: &str,
    ) -> Option<Arc<dyn maho_ext_api::ModelRegistry>> {
        self.contexts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(session_id)
            .map(|context| context.model_registry.clone())
    }

    /// Config-driven people gate for `/palace`, mirroring upstream `resolvePalacePeople`
    /// (`wiring.ts`): the resolved `memory.people` block drives the viewer's people panel and caps.
    /// Fields the resolved block omits fall back to the schema defaults.
    pub(crate) fn palace_people_options(
        &self,
    ) -> Option<maho_omo_memory::palace::people::PalacePeopleOptions> {
        let settings = match self.settings() {
            Ok(settings) => settings,
            Err(error) => {
                (self.host.warn)(&error);
                return None;
            }
        };
        let people = &settings["people"];
        let mut options = maho_omo_memory::palace::people::PalacePeopleOptions::default();
        if let Some(enabled) = people["enabled"].as_bool() {
            options.enabled = enabled;
        }
        if let Some(max_entries) = people["max_entries"].as_u64() {
            options.limits.max_entries = max_entries as usize;
        }
        if let Some(max_entry_chars) = people["max_entry_chars"].as_u64() {
            options.limits.max_entry_chars = max_entry_chars as usize;
        }
        Some(options)
    }

    fn context(&self) -> Option<ExtensionContext> {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn identity(&self, session: &str) -> Option<MemoryIdentityContext> {
        self.component
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(session)
            .filter(|state| state.enabled)
            .and_then(|state| state.context.clone())
    }

    fn active_identity(&self) -> Option<MemoryIdentityContext> {
        self.context()
            .and_then(|context| self.identity(context.session_manager.session_id()))
    }

    fn worker(&self, identity: &MemoryIdentityContext) -> Result<Arc<IdentityWorker>, String> {
        let mut workers = self.workers.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(worker) = workers.get(&identity.identity) {
            return Ok(worker.clone());
        }
        let store = maho_omo_memory::identity_runtime::create_identity_reservation_store(
            identity,
            &self.settings()?,
        )?;
        let worker = Arc::new(IdentityWorker {
            identity: identity.clone(),
            store,
            runner: tokio::sync::Mutex::new({
                let mut runner = worker::runner::SenpiSubprocessRunner::default();
                runner.admission_memory_root = identity
                    .durable_root
                    .ancestors()
                    .nth(2)
                    .map(std::path::Path::to_path_buf);
                runner
            }),
            cache: tokio::sync::Mutex::new(Default::default()),
            sandbox: Mutex::new(Default::default()),
        });
        workers.insert(identity.identity.clone(), worker.clone());
        Ok(worker)
    }

    fn facts(
        self: &Arc<Self>,
        identity: &MemoryIdentityContext,
        context: &ExtensionContext,
    ) -> Result<maho_omo_memory::facts_wiring::MemoryFactsWiringOptions, String> {
        self.capture_context(context);
        let settings = self.settings()?;
        let people = &settings["people"];
        let this = self.clone();
        let registry =
            maho_omo_memory::model_registry_resolver::resolve_native_memory_model_registry(context);
        let sandbox_host = self.clone();
        let sandbox_identity = identity.identity.clone();
        let facts_identity = identity.identity.clone();
        let facts_session =
            context
                .model
                .as_ref()
                .map(|model| worker::resolve_model::ReflectionSessionModel {
                    provider: model.provider.clone(),
                    id: model.id.clone(),
                    thinking: None,
                });
        let attempt = NativeFactsAttemptOptions {
            resolve_model: Arc::new(move || {
                maho_omo_memory::facts_model::resolve_facts_model(
                    &(this.host.load_config)()?,
                    Some(&registry),
                    facts_session.as_ref(),
                    &facts_identity,
                )
                .map_err(|error| error.to_string())
            }),
            env: self.child_env(),
            config_sources: (self.host.config_sources)(),
            launch: self.host.launcher.clone(),
            supervisor_command: self.host.supervisor_command.clone(),
            supervisor_args: self.host.supervisor_args.clone(),
            deadline_ms: 15 * 60_000,
            termination_grace_ms: 5_000,
            max_output_bytes: 64 * 1024,
            people: memory_core::facts::person_routing::FactsPeopleRouting {
                enabled: people["enabled"]
                    .as_bool()
                    .ok_or("people.enabled missing")?,
                max_entries: people["max_entries"]
                    .as_u64()
                    .ok_or("people.max_entries missing")? as usize,
                max_entry_chars: people["max_entry_chars"]
                    .as_u64()
                    .ok_or("people.max_entry_chars missing")?
                    as usize,
            },
            sandbox: Some(Arc::new(move |args| {
                let settings = sandbox_host.settings()?;
                let reflection =
                    maho_omo_memory::reflection_settings::resolve_agent_reflection_settings(
                        Some(&settings),
                        &sandbox_identity,
                    )?;
                let broker_endpoint = args
                    .env
                    .get("MAHO_FACTS_LOCK_BROKER")
                    .ok_or("facts broker endpoint missing")?;
                let broker_dir = std::path::Path::new(broker_endpoint)
                    .parent()
                    .ok_or("facts broker directory missing")?
                    .to_owned();
                maho_omo_memory::sandbox::apply_facts_sandbox(
                    args,
                    maho_omo_memory::sandbox::FactsSpawnSandboxInput {
                        policy: sandbox_policy(&reflection)?,
                        agent_dir: &sandbox_host.host.agent_dir,
                        broker_dir: &broker_dir,
                        foreign_roots: &[],
                        platform: platform(),
                    },
                    sandbox_host.host.which.as_ref(),
                    |message, _| (sandbox_host.host.warn)(message),
                )
                .map_err(|error| error.to_string())
            })),
            warn: self.host.warn.clone(),
            spawner: Some(
                crate::cli::facts_child::CliFactsChildSpawner::from_agent_dir(
                    self.host.agent_dir.to_string_lossy().into_owned(),
                ),
            ),
        };
        let this = self.clone();
        Ok(MemoryRuntimeWiring::native_facts_options(
            identity,
            Arc::new(move || this.settings()),
            attempt,
            Arc::new(memory_core::support::time::now_millis),
        ))
    }

    /// The factory is repeatable: options share all retained state on reload.
    pub fn options(self: &Arc<Self>) -> MemoryExtensionOptions {
        let this = self.clone();
        let facts = Arc::new(
            move |identity: &MemoryIdentityContext, context: &ExtensionContext| {
                this.facts(identity, context)
            },
        );
        let this = self.clone();
        let reconcile = Arc::new(move |identity: MemoryIdentityContext| -> maho_omo_memory::facts_wiring::FactsExtractorWork {
            let this = this.clone();
            Box::pin(async move {
                let worker = this.worker(&identity)?;
                // Recovery futures in the owner crate are thread-local. Run the
                // native recovery on a blocking worker with its own Tokio driver.
                tokio::task::spawn_blocking(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
                    runtime.block_on(this.reconcile_worker(&worker))
                }).await.map_err(|error| error.to_string())?
            })
        });
        let this = self.clone();
        let initial_status = Arc::new(
            move |identity: &MemoryIdentityContext, context: &ExtensionContext| {
                if let Err(error) = this.refresh_status(identity, context, StatusRefresh::Initial) {
                    (this.host.warn)(&error);
                }
            },
        );
        MemoryExtensionOptions {
            component: self.component.clone(),
            wiring: self.wiring.clone(),
            wiring_options: NativeMemoryWiringOptions {
                facts,
                reconcile,
                initial_status,
                now: Arc::new(|| memory_core::support::time::now_millis() as f64),
                warn: self.host.warn.clone(),
            },
            static_options: self.static_options(),
        }
    }

    /// The retained OMO list entry for this runtime: a fresh `MemoryExtension`
    /// is built per register (so OMO reload/recreate re-registers) but always
    /// over this ONE component/store/wiring and this runtime's live resolved
    /// settings. Build it before extension loading and keep the runtime alive.
    pub fn component(self: &Arc<Self>) -> maho_omo::OmoSenpiComponent {
        let this = self.clone();
        maho_omo::memory_component_from(move || this.options())
    }

    fn static_options(self: &Arc<Self>) -> MemoryStaticOptions {
        let mut slot = self
            .static_options
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut options = slot
            .get_or_insert_with(|| self.build_static_options())
            .clone();
        // pin `memory/index.ts:66,86`: every register re-reads the boot config for `toolExposure`;
        // only the shared Arcs stay cached.
        options.tool_exposure = self.tool_exposure();
        options
    }

    fn tool_exposure(&self) -> Option<maho_ext_api::ToolExposure> {
        match self.settings() {
            Ok(settings) => (settings["tool_exposure"] == "search")
                .then_some(maho_ext_api::ToolExposure::Search),
            Err(error) => {
                (self.host.warn)(&error);
                None
            }
        }
    }

    fn build_static_options(self: &Arc<Self>) -> MemoryStaticOptions {
        let this = self.clone();
        let resolve: maho_omo_memory::prompt::PromptContextResolver =
            Arc::new(move |session| this.identity(session));
        let this = self.clone();
        let nudge = Arc::new(move |repo: &GitMemoryRepo, session: &str, identity: &str| {
            let settings = this.settings()?;
            let setting = &settings["nudge"];
            let override_ = &settings["agents"][identity]["nudge"];
            this.nudge
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .nudge_turns(
                    repo,
                    session,
                    &maho_omo_memory::nudge_wiring::ResolvedNudgeSettings {
                        enabled: override_["enabled"]
                            .as_bool()
                            .or_else(|| setting["enabled"].as_bool())
                            .ok_or("nudge.enabled missing")?,
                        every_user_turns: override_["every_user_turns"]
                            .as_i64()
                            .or_else(|| setting["every_user_turns"].as_i64())
                            .ok_or("nudge threshold missing")?,
                    },
                )
                .map_err(|error| error.to_string())?
                .map(|turns| usize::try_from(turns).map_err(|error| error.to_string()))
                .transpose()
        });
        let soul_context = resolve.clone();
        let soul = Arc::new(move |repo: &GitMemoryRepo, session: &str, _: &str| {
            let identity = soul_context(session).ok_or("no bound memory session")?;
            memory_core::soul::watermark::consume_soul_notice_delta(
                repo,
                &memory_core::soul::watermark::ConsumeSoulNoticeOptions {
                    notices_dir: identity.identity_paths.notices,
                    locks_dir: identity.identity_paths.locks,
                    wait_timeout_ms: Some(2_000),
                },
            )
            .map(|notice| notice.map(|notice| notice.sha))
            .map_err(|error| error.to_string())
        });
        let this = self.clone();
        let search = Arc::new(move || match this.settings() {
            Ok(settings) => settings["tool_exposure"] == "search",
            Err(error) => {
                (this.host.warn)(&error);
                false
            }
        });
        let tool_exposure = self.tool_exposure();
        let this = self.clone();
        let tools = Arc::new(move || this.active_identity());
        let this = self.clone();
        let write_notice: maho_omo_memory::tools::WriteNoticeGate = Arc::new(move || {
            let identity = this.active_identity();
            let settings = this.settings().ok();
            let configured_agent = settings
                .as_ref()
                .and_then(|settings| settings["agent"].as_str());
            let identity_str =
                configured_agent.or_else(|| identity.as_ref().map(|id| id.identity.as_str()));
            maho_omo_memory::reflection_settings::resolve_write_notice_enabled(
                settings.as_ref(),
                identity_str,
            )
        });
        let cwd = self.host.cwd.clone();
        let resolve_cwd = Arc::new(move || cwd.clone());
        let this = self.clone();
        let edit_notice = Arc::new(move |identity: &str| match this.settings() {
            Ok(settings) => settings["agents"][identity]["soul"]["edit_notice"]
                .as_bool()
                .or_else(|| settings["soul"]["edit_notice"].as_bool())
                .unwrap_or(true),
            Err(error) => {
                (this.host.warn)(&error);
                false
            }
        });
        let this = self.clone();
        let writes = Arc::new(move |session: &str| {
            let identity = this.identity(session)?;
            Some(
                this.write_sessions
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .entry(session.into())
                    .or_insert_with(|| {
                        Arc::new(Mutex::new(
                            maho_omo_memory::wiring_memory_write::MemoryWriteSession {
                                context: identity,
                                memory_status_attempted: false,
                            },
                        ))
                    })
                    .clone(),
            )
        });
        let this = self.clone();
        let refresh = Arc::new(
            move |identity: &MemoryIdentityContext, context: &ExtensionContext, _sync: Option<&maho_omo_memory::sync_status_cached::SyncStatusReader>| {
                this.refresh_status(identity, context, StatusRefresh::Footer)
            },
        );
        let this = self.clone();
        let on_write = Arc::new(move |_: &str| {
            // pin `wiring.ts:133` onMemoryWrite: reflectionLive.syncRpc. The listener runs inside
            // the async ToolResult dispatch, so the wiring lock is only tried, never awaited.
            match this.wiring.try_lock() {
                Ok(mut wiring) => wiring.sync_rpc(memory_core::support::time::now_millis()),
                Err(_) => Ok(()),
            }
        });
        let this = self.clone();
        let skills = Arc::new(move |_: &maho_ext_api::ToolCallEvent| this.active_identity());
        let this = self.clone();
        let triggers = Arc::new(move |session: Option<&str>| {
            let session = session.map(str::to_owned).or_else(|| {
                this.context()
                    .map(|context| context.session_manager.session_id().to_owned())
            })?;
            let identity = this.identity(&session)?;
            match this.trigger_session(&session, &identity) {
                Ok(session) => Some(session),
                Err(error) => {
                    (this.host.warn)(&error);
                    None
                }
            }
        });
        let this = self.clone();
        let launch = Arc::new(move |request: memory_core::reflection::ReflectionRequest| {
            let identity = this.active_identity().ok_or("no bound memory session")?;
            let worker = this.worker(&identity)?;
            let active =
                memory_core::reflection::ReflectionReservationStore::read_state(&worker.store)
                    .map_err(|error| error.to_string())?
                    .active
                    .ok_or("no active reflection reservation")?;
            if active.request != request {
                return Err("reflection request does not match active reservation".into());
            }
            this.launch(worker, active, true)
        });
        MemoryStaticOptions {
            register_host: Some({
                let this = self.clone();
                Arc::new(move |api| this.register_host(api))
            }),
            skills_trackers: Some(self.skills.clone()),
            prompt_handler: Some(self.prompt.clone()),
            prompt: maho_omo_memory::prompt::MemoryPromptInjectionOptions {
                resolve_context: resolve.clone(),
                create_repo: Some(Arc::new(|identity| {
                    GitMemoryRepo::open(&identity.identity_paths.repo, &identity.identity)
                        .map_err(|error| error.to_string())
                })),
                search_exposure: Some(search),
                resolve_nudge_turns: Some(nudge),
                resolve_soul_notice: Some(soul),
            },
            nudge: self.nudge.clone(),
            resolve_context: resolve,
            resolve_tool_context: tools,
            write_notice: Some(write_notice),
            tool_exposure,
            resolve_cwd: resolve_cwd.clone(),
            captured_tools: self.host.captured_tools.clone(),
            warn: self.host.warn.clone(),
            edit_notice,
            theme: Arc::new(|theme| Arc::new(MemoryEntryTheme(theme.clone()))),
            memory_write: maho_omo_memory::wiring_memory_write::MemoryWriteOptions {
                resolve_session: writes,
                on_memory_write: on_write,
                refresh_status: refresh,
                sync_status: None,
            },
            skills_usage: maho_omo_memory::skills_usage_wiring::SkillsUsageOptions {
                resolve_context: skills,
                resolve_cwd,
                now_ms: Arc::new(|| memory_core::support::time::now_millis().max(0) as u64),
            },
            triggers: maho_omo_memory::trigger_wiring::ReflectionTriggerWiringOptions {
                resolve_session: triggers,
                on_launch: launch,
                warn: self.host.warn.clone(),
            },
            device_name: None,
            load_settings: Some({
                let this = self.clone();
                Arc::new(move || this.settings().unwrap_or_default())
            }),
        }
    }

    fn trigger_session(
        &self,
        session: &str,
        identity: &MemoryIdentityContext,
    ) -> Result<maho_omo_memory::trigger_wiring::ReflectionTriggerSession, String> {
        let worker = self.worker(identity)?;
        let enabled = maho_omo_memory::trigger_wiring::resolve_reflection_trigger_config(
            &self.settings()?,
            Some(&identity.identity),
        )?
        .enabled;
        // pin `context.ts:34-36`: the trigger mutates the bound context's shared ledger, the same
        // object the RPC snapshot reads; a new bind starts with a fresh ledger.
        let ledger = identity.ledger.clone();
        Ok(maho_omo_memory::trigger_wiring::ReflectionTriggerSession {
            conversation_id: session.into(),
            ledger,
            engine: worker.store.clone(),
            enabled,
        })
    }

    fn refresh_status(
        &self,
        identity: &MemoryIdentityContext,
        context: &ExtensionContext,
        mode: StatusRefresh,
    ) -> Result<maho_omo_memory::status::MemoryStatusResult, String> {
        let settings = self.settings()?;
        let repo = GitMemoryRepo::open(&identity.identity_paths.repo, &identity.identity)
            .map_err(|error| error.to_string())?;
        let result = maho_omo_memory::status::refresh_memory_status(
            &repo,
            &mut StatusUi(context.ui.clone()),
            &maho_omo_memory::status::RefreshMemoryStatusInput {
                context: identity,
                compile_warn_tokens: settings["compile_warn_tokens"]
                    .as_u64()
                    .ok_or("compile_warn_tokens missing")?
                    as usize,
                already_notified: false,
                now_ms: memory_core::support::time::now_millis(),
                show_footer: context.has_ui,
                check_advisory: mode == StatusRefresh::Initial,
                sync_status: self
                    .wiring
                    .try_lock()
                    .ok()
                    .and_then(|wiring| wiring.seams.sync_status.clone())
                    .as_ref(),
                session_id: Some(context.session_manager.session_id()),
            },
        )
        .map_err(|error| error.to_string())?;
        Ok(result)
    }

    /// `announce` marks the trigger/manual launch sites that fire the reflection-launched hook
    /// (pin `wiring-runtime.ts:186`, `wiring-static.ts:162`); chained, recovery and dream launches do not.
    fn launch(
        self: &Arc<Self>,
        worker: Arc<IdentityWorker>,
        run: ReservedRun,
        announce: bool,
    ) -> Result<(), String> {
        let this = self.clone();
        std::thread::Builder::new()
            .name(format!("memory-{}", run.run_id))
            .spawn(move || {
                if announce {
                    this.announce_launch(&worker.identity.identity, &run);
                }
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())
                    .and_then(|runtime| runtime.block_on(this.run_worker(&worker, run)));
                if let Err(error) = result {
                    (this.host.warn)(&format!("memory reflection launch failed: {error}"));
                }
            })
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Runs on the launch thread before its runtime starts, so the blocking wiring lock is legal.
    fn announce_launch(self: &Arc<Self>, identity: &str, run: &ReservedRun) {
        let category = self
            .settings()
            .and_then(|settings| {
                maho_omo_memory::reflection_settings::resolve_agent_reflection_settings(
                    Some(&settings),
                    identity,
                )
            })
            .map(|reflection| {
                reflection["category"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            });
        let category = match category {
            Ok(category) => category,
            Err(error) => {
                (self.host.warn)(&error);
                String::new()
            }
        };
        let trigger = serde_json::to_value(run.request.trigger)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default();
        let now = memory_core::support::time::now_millis();
        let details = maho_omo_memory::status_active_runs::ActiveReflectionRunDetails {
            trigger,
            category,
            model: None,
            started_at: run
                .reserved_at
                .clone()
                .unwrap_or_else(|| memory_core::support::time::format_rfc3339_millis(now)),
        };
        let start_ticker = self.wiring.blocking_lock().on_reflection_launched(
            identity,
            &run.run_id,
            details,
            now,
            self.host.warn.as_ref(),
        );
        if start_ticker
            && let Err(error) = maho_omo_memory::wiring::spawn_footer_ticker(self.wiring.clone())
        {
            (self.host.warn)(&format!("memory footer ticker failed: {error}"));
        }
    }

    async fn run_worker(
        self: &Arc<Self>,
        worker: &IdentityWorker,
        mut run: ReservedRun,
    ) -> Result<(), String> {
        loop {
            let context = self.context().ok_or("no captured memory context")?;
            let config = (self.host.load_config)()?;
            let settings = self.settings()?;
            let reflection =
                maho_omo_memory::reflection_settings::resolve_agent_reflection_settings(
                    Some(&settings),
                    &worker.identity.identity,
                )?;
            let registry =
                maho_omo_memory::model_registry_resolver::resolve_native_memory_model_registry(
                    &context,
                );
            let session_model =
                context
                    .model
                    .as_ref()
                    .map(|model| worker::resolve_model::ReflectionSessionModel {
                        provider: model.provider.clone(),
                        id: model.id.clone(),
                        thinking: None,
                    });
            let category = reflection["category"]
                .as_str()
                .ok_or("reflection.category missing")?;
            let resolution = worker::resolve_model::resolve_reflection_model(
                category,
                &config,
                Some(&registry),
                session_model.as_ref(),
            )
            .map_err(|error| error.to_string())?;
            let route = if matches!(
                resolution,
                worker::resolve_model::ReflectionModelResolution::Resolved { .. }
            ) {
                Some(worker::runner::SenpiSubprocessRunner::choose_launch_route(&run, &resolution, Some(&registry), session_model.as_ref(),
                    maho_omo_memory::session_context_resolver::resolve_native_parent_context_tokens(&context).map_err(|error| error.to_string())?, (self.host.parent_cache_reusable)(&context))?)
            } else {
                None
            };
            let identity = maho_omo_memory::identity_runtime::as_memory_identity(&worker.identity);
            let now = memory_core::support::time::now_millis();
            let started = memory_core::support::time::format_rfc3339_millis(now);
            let sources = (self.host.config_sources)();
            let sandbox = |args| {
                worker
                    .sandbox
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .apply(
                        maho_omo_memory::identity_runtime::IdentitySandboxInput {
                            identity: &worker.identity,
                            policy: sandbox_policy(&reflection)?,
                            agent_dir: &self.host.agent_dir,
                            platform: platform(),
                        },
                        args,
                        self.host.which.as_ref(),
                        |message| (self.host.warn)(message),
                    )
            };
            let ensure_renderer = || (self.host.ensure_completion_renderer)();
            let health_alert = |dir: &Path| {
                let mut live = LiveSession {
                    runtime: self,
                    context: &context,
                    identity: &identity.id,
                };
                worker::health_alert::emit_reflection_health_alert(
                    dir,
                    &identity.id,
                    Some(&mut live),
                    &mut |key| {
                        self.health_notices
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .insert(key.into())
                    },
                    memory_core::support::time::now_millis(),
                );
            };
            let append_launched = || {
                let mut live = LiveSession {
                    runtime: self,
                    context: &context,
                    identity: &identity.id,
                };
                worker::runner::SenpiSubprocessRunner::append_launched(
                    &run,
                    &identity,
                    &resolution,
                    &started,
                    Some(&mut live),
                    |conversation| {
                        memory_core::journal::store::TranscriptJournal::new(
                            memory_core::journal::store::TranscriptJournalOptions::new(
                                identity.paths.transcripts.join(conversation),
                            ),
                        )
                        .get_state()
                        .map(Some)
                        .map_err(|error| error.to_string())
                    },
                )
            };
            let mut live = LiveSession {
                runtime: self,
                context: &context,
                identity: &identity.id,
            };
            let env = self.child_env();
            let mut runner = worker.runner.lock().await;
            let mut cache = worker.cache.lock().await;
            let dir = identity.paths.reflection.join("runs").join(&run.run_id);
            let result = runner
                .launch_native(
                    worker::runner::ReflectionRunnerInput {
                        run: &run,
                        identity: &identity,
                        config: &config,
                        resolution: &resolution,
                        reservation: worker.store.as_ref(),
                        started_at: &started,
                        now_ms: &memory_core::support::time::now_millis,
                    },
                    &mut cache,
                    worker::runner::NativeReflectionExecutionOptions {
                        env: &env,
                        sources: &sources,
                        launcher: &self.host.launcher,
                        parent_session_file: context.session_manager.session_file(),
                        parent_cwd: Some(&self.host.cwd),
                        route: route.as_ref(),
                        sandbox: Some(&sandbox),
                        deadline_ms: None,
                        memory_root: None,
                        child: worker::spawn_supervisor::ReflectionChildOptions {
                            termination_grace_ms: 5_000.0,
                            max_output_bytes: 64 * 1024,
                            supervisor_command: &self.host.supervisor_command,
                            supervisor_args: &self.host.supervisor_args,
                            launched_at: now,
                        },
                        callbacks: worker::runner::NativeReflectionCallbacks {
                            ensure_renderer: &ensure_renderer,
                            health_alert: &health_alert,
                            append_launched: &append_launched,
                            warn: self.host.warn.as_ref(),
                        },
                    },
                    Some(&mut live),
                    |operation| terminal_gate(&dir, &run.run_id, operation),
                )
                .await?;
            drop(cache);
            drop(runner);
            match result.launch {
                Some(next) => run = next,
                None => return Ok(()),
            }
        }
    }

    async fn reconcile_worker(
        self: &Arc<Self>,
        worker: &Arc<IdentityWorker>,
    ) -> Result<(), String> {
        let identity = maho_omo_memory::identity_runtime::as_memory_identity(&worker.identity);
        let launch = |run: &ReservedRun| {
            if let Err(error) = self.launch(worker.clone(), run.clone(), false) {
                (self.host.warn)(&error);
            }
        };
        let receipts = maho_omo_memory::receipts_port::DefaultReceiptsPort;
        let warn = |message: &str, _fields: &BTreeMap<String, String>| (self.host.warn)(message);
        let context = worker::run_finalization_types::RunFinalizationContext {
            identity: &identity,
            reservation: worker.store.as_ref(),
            launch: Some(&launch),
            now_ms: &memory_core::support::time::now_millis,
            receipts: Some(&receipts),
            warn: Some(&warn),
        };
        let gate = |dir: &Path,
                    run: &str,
                    operation: &mut dyn FnMut() -> Result<
            Option<worker::run_finalization_types::ReservationRunResult>,
            String,
        >| terminal_gate(dir, run, operation);
        let recovery = worker::run_reconciliation::NativeReflectionRecovery {
            context: &context,
            terminal_gate: &gate,
        };
        let sweep_logger =
            worker::run_reconciliation_sweep::ReflectionSweepWarnLogger(self.host.warn.as_ref());
        worker::run_reconciliation::reconcile_reflection_runs_with_options(
            &context,
            &recovery,
            Some(&sweep_logger),
            true,
        )
        .await
        .map(|_| ())
    }
}

fn terminal_gate<T>(
    dir: &Path,
    run: &str,
    operation: &mut dyn FnMut() -> Result<T, String>,
) -> Result<T, String> {
    worker::run_terminal_gate::with_run_terminal_gate(
        dir,
        run,
        operation,
        |path, record, operation| {
            // Source uses an unbounded native lock, not a bounded failure fallback.
            loop {
                match memory_core::locks::acquire_lock(
                    path,
                    record,
                    &memory_core::locks::AcquireLockOptions {
                        wait_timeout_ms: Some(60_000),
                        ..Default::default()
                    },
                ) {
                    Ok(()) => break,
                    Err(memory_core::locks::AcquireLockError::Contention(_)) => continue,
                    Err(error) => return Err(error.to_string()),
                }
            }
            let result = operation();
            memory_core::locks::release_lock(path, record).map_err(|error| error.to_string())?;
            result
        },
    )
}

fn sandbox_policy(settings: &Value) -> Result<maho_omo_memory::sandbox::SandboxPolicy, String> {
    match settings["sandbox"].as_str() {
        Some("required") => Ok(maho_omo_memory::sandbox::SandboxPolicy::Required),
        Some("auto") => Ok(maho_omo_memory::sandbox::SandboxPolicy::Auto),
        Some("off") => Ok(maho_omo_memory::sandbox::SandboxPolicy::Off),
        _ => Err("reflection.sandbox missing or invalid".into()),
    }
}
fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "win32"
    } else {
        "linux"
    }
}

/// `Initial` is bind-time refreshInitialStatus (advisory on); `Footer` is the write-listener and
/// completion refresh with `checkAdvisory: false` (pin `wiring-memory-write.ts`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum StatusRefresh {
    Initial,
    Footer,
}
struct StatusUi(Arc<dyn ExtensionUi>);
impl maho_omo_memory::status::MemoryStatusUi for StatusUi {
    fn set_status(&mut self, key: &str, text: Option<&str>) {
        self.0.set_status(key, text);
    }
    fn notify(&mut self, message: &str, _: &str) {
        self.0.notify(message, NotificationType::Warning);
    }
}
struct MemoryEntryTheme(maho_ext_api::Theme);
impl worker::entry_renderers::EntryRenderTheme for MemoryEntryTheme {
    // The exported extension theme already carries ANSI SGR prefixes, so senpi's `fg`/`bg` apply them directly.
    fn fg(&self, tone: &str, text: &str) -> String {
        self.0.fg(tone, text)
    }
    fn italic(&self, text: &str) -> String {
        format!("\x1b[3m{text}\x1b[23m")
    }
    fn bg(&self, name: &str, text: &str) -> String {
        self.0.bg(name, text)
    }
}
struct LiveSession<'a> {
    runtime: &'a MemoryRuntime,
    context: &'a ExtensionContext,
    identity: &'a str,
}
impl worker::completion_delivery::ReflectionLiveSession for LiveSession<'_> {
    fn session_id(&self) -> &str {
        self.context.session_manager.session_id()
    }
    fn append_entry(&mut self, kind: &str, data: Value) {
        if let Err(error) = self.runtime.host.actions.append_entry(kind, Some(data)) {
            (self.runtime.host.warn)(&error.to_string());
        }
    }
    fn notify(&mut self, message: &str, warning: bool) -> Result<(), String> {
        self.context.ui.notify(
            message,
            if warning {
                NotificationType::Warning
            } else {
                NotificationType::Info
            },
        );
        Ok(())
    }
    fn warn(&mut self, message: &str, error: &str) {
        (self.runtime.host.warn)(&format!("{message}: {error}"));
    }
    fn on_completion(&mut self, run_id: &str) {
        // pin `wiring-runtime.ts:160` onLiveCompletion -> onLiveReflectionCompleted. Delivery runs
        // inside the worker's async runtime, so the wiring lock is taken on a helper thread.
        let wiring = self.runtime.wiring.clone();
        let warn = self.runtime.host.warn.clone();
        let identity = self.identity.to_owned();
        let run_id = run_id.to_owned();
        let spawned = std::thread::Builder::new()
            .name("memory-completion".into())
            .spawn(move || {
                if let Err(error) = wiring.blocking_lock().on_live_reflection_completed(
                    &identity,
                    &run_id,
                    memory_core::support::time::now_millis(),
                ) {
                    warn(&error);
                }
            });
        if let Err(error) = spawned {
            (self.runtime.host.warn)(&error.to_string());
        }
    }
}
impl worker::health_alert::ReflectionHealthLiveSession for LiveSession<'_> {
    fn session_id(&self) -> &str {
        self.context.session_manager.session_id()
    }
    fn has_ui(&self) -> bool {
        self.context.has_ui
    }
    fn append_entry(&mut self, kind: &str, entry: &worker::health_alert::ReflectionHealthEntry) {
        match serde_json::to_value(entry) {
            Ok(data) => {
                worker::completion_delivery::ReflectionLiveSession::append_entry(self, kind, data)
            }
            Err(error) => (self.runtime.host.warn)(&error.to_string()),
        }
    }
    fn notify(&mut self, message: &str, _: &str) {
        self.context.ui.notify(message, NotificationType::Warning);
    }
}

/// Adapts the retained dream trigger wiring to the command-layer `/dream` sink.
///
/// Maps `DreamFireOutcome` onto `DreamCommandOutcome` and a missing active session
/// (`None`) onto the pinned `no_session` rejection string.
struct DreamSink(Arc<maho_omo_memory::dream_trigger::DreamTriggerWiring>);

impl DreamRequestSink for DreamSink {
    fn request(
        &self,
        request: ManualDreamCommandRequest,
    ) -> maho_omo_memory::commands::types::BoxFuture<'_, Result<DreamCommandOutcome, String>> {
        let wiring = self.0.clone();
        Box::pin(async move {
            let outcome = wiring
                .request_manual_dream(maho_omo_memory::dream_trigger::ManualDreamRequest {
                    focus: request.focus,
                    conversation_ids: request.conversation_ids,
                    target_doc: request.target_doc,
                    deadline_at: None,
                })
                .await?;
            Ok(match outcome {
                Some(maho_omo_memory::dream_trigger::DreamFireOutcome::Fired {
                    run_id,
                    status,
                }) => DreamCommandOutcome::Fired { run_id, status },
                Some(maho_omo_memory::dream_trigger::DreamFireOutcome::Rejected(rejection)) => {
                    DreamCommandOutcome::Rejected {
                        rejection: dream_rejection_code(rejection),
                    }
                }
                None => DreamCommandOutcome::Rejected {
                    rejection: "no_session".to_owned(),
                },
            })
        })
    }
}

fn dream_rejection_code(
    rejection: maho_omo_memory::dream_trigger_fire::DreamFireRejection,
) -> String {
    use maho_omo_memory::dream_trigger_fire::DreamFireRejection;
    match rejection {
        DreamFireRejection::NoUnreflectedContent => "no_unreflected_content".to_owned(),
        DreamFireRejection::Aborted => "aborted".to_owned(),
        DreamFireRejection::Parked => "parked".to_owned(),
        DreamFireRejection::Gate(gate) => gate.as_code().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoopActions;
    impl ExtensionActions for NoopActions {
        fn send_message(
            &self,
            _: maho_ext_api::CustomMessage,
            _: maho_ext_api::SendMessageOptions,
        ) -> Result<(), maho_ext_api::ExtensionFailure> {
            Ok(())
        }
        fn send_user_message(
            &self,
            _: maho_ext_api::UserMessageContent,
            _: maho_ext_api::SendUserMessageOptions,
        ) -> Result<(), maho_ext_api::ExtensionFailure> {
            Ok(())
        }
        fn append_entry(
            &self,
            _: &str,
            _: Option<Value>,
        ) -> Result<(), maho_ext_api::ExtensionFailure> {
            Ok(())
        }
        fn get_all_tools(
            &self,
        ) -> Result<Vec<maho_ext_api::ToolInfo>, maho_ext_api::ExtensionFailure> {
            Ok(Vec::new())
        }
    }

    /// A host whose only live input is the shared config cell; nothing launches.
    fn host(config: Arc<Mutex<Value>>) -> MemoryRuntimeHost {
        let load: LiveMemoryConfig = {
            let config = config.clone();
            Arc::new(move || {
                Ok(config
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone())
            })
        };
        MemoryRuntimeHost {
            cwd: PathBuf::from("/project"),
            agent_dir: PathBuf::from("/agent"),
            env: BTreeMap::new(),
            load_config: load,
            config_sources: Arc::new(Vec::new),
            launcher: worker::model_preflight::Launcher {
                command: "unused".into(),
                prefix_args: vec![],
            },
            supervisor_command: PathBuf::from("unused"),
            supervisor_args: vec![],
            actions: Arc::new(NoopActions),
            ensure_completion_renderer: Arc::new(|| {}),
            captured_tools: Arc::new(Vec::new),
            disabled: Arc::new(|| false),
            parent_cache_reusable: Arc::new(|_| false),
            which: Arc::new(|_| None),
            warn: Arc::new(|_| {}),
        }
    }

    fn runtime(config: Value) -> Arc<MemoryRuntime> {
        MemoryRuntime::new(host(Arc::new(Mutex::new(config)))).unwrap()
    }

    #[test]
    fn rejects_envelope_config() {
        assert!(
            MemoryRuntime::new(host(Arc::new(Mutex::new(
                serde_json::json!({ "global": {} })
            ))))
            .is_err()
        );
        assert!(
            MemoryRuntime::new(host(Arc::new(Mutex::new(
                serde_json::json!({ "project": {} })
            ))))
            .is_err()
        );
    }

    #[test]
    fn reads_live_resolved_settings_not_a_frozen_snapshot() {
        let config = Arc::new(Mutex::new(
            serde_json::json!({ "memory": { "soul": { "edit_notice": false } } }),
        ));
        let runtime = MemoryRuntime::new(host(config.clone())).unwrap();
        assert_eq!(
            runtime.settings().unwrap()["soul"]["edit_notice"],
            serde_json::json!(false)
        );
        *config.lock().unwrap_or_else(PoisonError::into_inner) =
            serde_json::json!({ "memory": { "soul": { "edit_notice": true } } });
        assert_eq!(
            runtime.settings().unwrap()["soul"]["edit_notice"],
            serde_json::json!(true)
        );
    }

    #[test]
    fn options_reread_tool_exposure_on_every_build() {
        let config = Arc::new(Mutex::new(serde_json::json!({ "memory": {} })));
        let runtime = MemoryRuntime::new(host(config.clone())).unwrap();
        assert_eq!(runtime.options().static_options.tool_exposure, None);
        *config.lock().unwrap_or_else(PoisonError::into_inner) =
            serde_json::json!({ "memory": { "tool_exposure": "search" } });
        assert_eq!(
            runtime.options().static_options.tool_exposure,
            Some(maho_ext_api::ToolExposure::Search)
        );
    }

    #[test]
    fn options_reuse_one_component_wiring_and_retained_ports() {
        let runtime = runtime(serde_json::json!({ "memory": {} }));
        let first = runtime.options();
        let second = runtime.options();
        assert!(Arc::ptr_eq(&first.component, &second.component));
        assert!(Arc::ptr_eq(&first.wiring, &second.wiring));
        let (first, second) = (first.static_options, second.static_options);
        assert!(Arc::ptr_eq(
            first.register_host.as_ref().unwrap(),
            second.register_host.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(
            first.skills_trackers.as_ref().unwrap(),
            second.skills_trackers.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(
            first.prompt_handler.as_ref().unwrap(),
            second.prompt_handler.as_ref().unwrap()
        ));
    }

    #[test]
    fn command_options_supply_the_bounded_system_git_executor() {
        let runtime = runtime(serde_json::json!({ "memory": {} }));
        let options = runtime.options();
        let deps = options.static_options.register_host;
        assert!(deps.is_some());
    }

    #[test]
    fn component_is_the_retained_memory_list_entry() {
        let runtime = runtime(serde_json::json!({ "memory": {} }));
        assert_eq!(runtime.component().name, "memory");
    }

    /// The memory host registers `/palace` through `register_host` (the CLI body of
    /// `register_memory_static`), so the command is reachable in production.
    #[test]
    fn register_host_installs_the_palace_command() {
        let runtime = runtime(serde_json::json!({ "memory": {} }));
        let mut api = maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("memory", Default::default(), Default::default()),
            Default::default(),
            Default::default(),
            Default::default(),
        );

        runtime.register_host(&mut api);

        let palace = api
            .registered
            .commands
            .iter()
            .find(|command| command.name == "palace")
            .expect("the /palace command is registered by the memory host");
        assert!(palace.description.is_some());
        assert!(Arc::strong_count(&palace.handler) >= 1);
    }

    /// The `/palace` people resolver is config-driven: it reads the resolved `memory.people` block
    /// rather than returning a constant, and falls back to the schema defaults when fields are absent.
    #[test]
    fn palace_people_options_follow_the_resolved_memory_people_block() {
        let defaults = runtime(serde_json::json!({ "memory": {} }))
            .palace_people_options()
            .expect("resolved people options");
        assert!(!defaults.enabled);
        assert_eq!(defaults.limits.max_entries, 40);
        assert_eq!(defaults.limits.max_entry_chars, 200);

        let configured = runtime(serde_json::json!({
            "memory": { "people": { "enabled": false, "max_entries": 5, "max_entry_chars": 50 } }
        }))
        .palace_people_options()
        .expect("resolved people options");
        assert!(!configured.enabled);
        assert_eq!(configured.limits.max_entries, 5);
        assert_eq!(configured.limits.max_entry_chars, 50);

        let enabled = runtime(serde_json::json!({
            "memory": { "people": { "enabled": true } }
        }))
        .palace_people_options()
        .expect("resolved people options");
        assert!(enabled.enabled);
        assert_eq!(enabled.limits.max_entries, 40);
        assert_eq!(enabled.limits.max_entry_chars, 200);
    }

    struct TestSession(&'static str);
    impl maho_ext_api::ToolSessionManager for TestSession {
        fn session_id(&self) -> &str {
            self.0
        }
        fn session_file(&self) -> Option<&Path> {
            None
        }
    }
    impl maho_ext_api::SessionManager for TestSession {
        fn get_entries(&self) -> Vec<maho_ext_api::SessionEntry> {
            Vec::new()
        }
        fn get_branch(&self) -> Vec<maho_ext_api::SessionEntry> {
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
    impl maho_ext_api::ModelRegistry for TestRegistry {
        fn get_all(&self) -> Vec<maho_ext_api::Model> {
            Vec::new()
        }
        fn get_available(&self) -> Vec<maho_ext_api::Model> {
            Vec::new()
        }
        fn find(&self, _: &str, _: &str) -> Option<maho_ext_api::Model> {
            None
        }
        fn has_configured_auth(&self, _: &maho_ext_api::Model) -> bool {
            false
        }
        fn get_api_key_for_provider<'a>(
            &'a self,
            _: &'a str,
        ) -> maho_ext_api::ExtensionFuture<'a, Option<String>> {
            Box::pin(async { Ok(None) })
        }
    }
    struct TestUi;
    impl ExtensionUi for TestUi {
        fn select<'a>(
            &'a self,
            _: &'a str,
            _: &'a [String],
            _: maho_ext_api::ExtensionUiDialogOptions,
        ) -> maho_ext_api::UiFuture<'a, Option<String>> {
            Box::pin(async { None })
        }
        fn confirm<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
            _: maho_ext_api::ExtensionUiDialogOptions,
        ) -> maho_ext_api::UiFuture<'a, bool> {
            Box::pin(async { false })
        }
        fn input<'a>(
            &'a self,
            _: &'a str,
            _: Option<&'a str>,
            _: maho_ext_api::ExtensionUiDialogOptions,
        ) -> maho_ext_api::UiFuture<'a, Option<String>> {
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
        ) -> maho_ext_api::ExtensionFuture<'_, Value> {
            Box::pin(async { Ok(serde_json::Value::Null) })
        }
        fn theme(&self) -> maho_ext_api::Theme {
            maho_ext_api::Theme::default()
        }
    }

    fn test_context(session_id: &'static str, cwd: &Path, agent_dir: &Path) -> ExtensionContext {
        ExtensionContext {
            ui: Arc::new(TestUi),
            mode: maho_ext_api::ExtensionMode::Print,
            has_ui: false,
            cwd: cwd.into(),
            agent_dir: agent_dir.into(),
            session_manager: Arc::new(TestSession(session_id)),
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
            get_system_prompt_options_fn: Arc::new(|| maho_ext_api::BuildSystemPromptOptions {
                cwd: "/tmp".into(),
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

    #[tokio::test]
    async fn registered_memory_tool_via_static_options_reflects_write_notice_settings_and_per_agent_override()
     {
        let root = tempfile::tempdir().unwrap();
        let agent_dir = root.path().join("agent");
        let project_dir = root.path().join("project");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::create_dir_all(&project_dir).unwrap();

        let config = Arc::new(Mutex::new(serde_json::json!({
            "memory": {
                "enabled": true,
                "agent": "lead",
                "write_notice": { "enabled": true },
                "agents": {
                    "override-off": { "write_notice": { "enabled": false } },
                    "override-on": { "write_notice": { "enabled": true } }
                }
            }
        })));

        let mut h = host(config.clone());
        h.cwd = project_dir.clone();
        h.agent_dir = agent_dir.clone();
        h.env
            .insert("HOME".into(), agent_dir.to_string_lossy().into_owned());
        let runtime = MemoryRuntime::new(h).unwrap();

        let mut api = maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("memory", Default::default(), Default::default()),
            Default::default(),
            Default::default(),
            Default::default(),
        );

        api.runtime.bind(Arc::new(NoopActions));
        // Register memory static options and component lifecycle handlers
        let static_options = runtime.static_options();
        maho_omo_memory::wiring_static::register_memory_static(&mut api, static_options);
        let registered = runtime
            .component
            .register(
                &mut api,
                maho_omo_memory::index::MemoryComponentHooks {
                    after_bind: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
                    shutdown: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
                    clear_status: Arc::new(|_| {}),
                    warn: Arc::new(|_| {}),
                },
                |_| {},
            )
            .expect("memory component registration");
        assert!(registered, "memory component should register successfully");

        // 1. Session start binds "session-1" with identity "lead" (configured agent: lead, no per-agent override)
        let ctx1 = test_context("session-1", &project_dir, &agent_dir);
        for handler in api
            .registered
            .handlers
            .get(&maho_ext_api::EventKind::SessionStart)
            .expect("SessionStart handlers")
        {
            (handler)(
                &mut maho_ext_api::ExtensionEvent::SessionStart(maho_ext_api::SessionStartEvent {
                    reason: maho_ext_api::SessionReason::Startup,
                    initial_model_provenance: None,
                    previous_session_file: None,
                    attended: None,
                }),
                &ctx1,
            )
            .await
            .expect("session start dispatch");
        }
        runtime.capture_context(&ctx1);
        let lead_identity = runtime
            .active_identity()
            .expect("bound active identity")
            .identity;
        assert!(
            lead_identity.starts_with("lead"),
            "session bound the configured lead agent: {lead_identity}"
        );

        let tool = api
            .registered
            .tools
            .iter()
            .find(|t| t.definition.name == "memory")
            .expect("memory tool registered");

        // Call 1: lead agent with global enabled: true and NO per-agent override -> writeNotice present with identity = lead
        let call1 = maho_ext_api::ToolCall {
            id: "c1",
            params: serde_json::json!({"command":"create","reason":"lead write","file_path":"reference/lead.md","description":"lead","file_text":"content1"}),
            signal: Default::default(),
            on_update: None,
            context: None,
        };
        let res1 = (tool.definition.execute)(call1)
            .await
            .expect("lead create succeeds");
        let details1 = res1.details.expect("details present");
        assert!(
            details1.get("writeNotice").is_some(),
            "writeNotice emitted for lead under global enabled: true"
        );
        assert_eq!(
            details1["writeNotice"]["identity"],
            serde_json::json!(lead_identity)
        );

        // 2. Change global write_notice setting to false (lead still has no override) -> next call reflects it without restart
        *config.lock().unwrap() = serde_json::json!({
            "memory": {
                "enabled": true,
                "agent": "lead",
                "write_notice": { "enabled": false },
                "agents": {
                    "override-off": { "write_notice": { "enabled": false } },
                    "override-on": { "write_notice": { "enabled": true } }
                }
            }
        });
        let call2 = maho_ext_api::ToolCall {
            id: "c2",
            params: serde_json::json!({"command":"create","reason":"lead write 2","file_path":"reference/lead2.md","description":"lead","file_text":"content2"}),
            signal: Default::default(),
            on_update: None,
            context: None,
        };
        let res2 = (tool.definition.execute)(call2)
            .await
            .expect("lead create 2 succeeds");
        let details2 = res2.details.expect("details present");
        assert!(
            details2.get("writeNotice").is_none(),
            "writeNotice omitted when global disabled and no override"
        );

        // 3. Switch configured agent to "override-on" (per-agent override enabled: true even though global is false)
        *config.lock().unwrap() = serde_json::json!({
            "memory": {
                "enabled": true,
                "agent": "override-on",
                "write_notice": { "enabled": false },
                "agents": {
                    "override-off": { "write_notice": { "enabled": false } },
                    "override-on": { "write_notice": { "enabled": true } }
                }
            }
        });
        let ctx2 = test_context("session-2", &project_dir, &agent_dir);
        for handler in api
            .registered
            .handlers
            .get(&maho_ext_api::EventKind::SessionStart)
            .expect("SessionStart handlers")
        {
            (handler)(
                &mut maho_ext_api::ExtensionEvent::SessionStart(maho_ext_api::SessionStartEvent {
                    reason: maho_ext_api::SessionReason::Startup,
                    initial_model_provenance: None,
                    previous_session_file: None,
                    attended: None,
                }),
                &ctx2,
            )
            .await
            .expect("session start dispatch");
        }
        runtime.capture_context(&ctx2);
        let override_on_identity = runtime
            .active_identity()
            .expect("bound active identity")
            .identity;
        assert!(
            override_on_identity.starts_with("override-on"),
            "session bound the override-on agent: {override_on_identity}"
        );

        let call3 = maho_ext_api::ToolCall {
            id: "c3",
            params: serde_json::json!({"command":"create","reason":"override on write","file_path":"reference/on.md","description":"on","file_text":"content3"}),
            signal: Default::default(),
            on_update: None,
            context: None,
        };
        let res3 = (tool.definition.execute)(call3)
            .await
            .expect("override-on create succeeds");
        let details3 = res3.details.expect("details present");
        assert!(
            details3.get("writeNotice").is_some(),
            "writeNotice emitted for override-on despite global false"
        );
        assert_eq!(
            details3["writeNotice"]["identity"],
            serde_json::json!(override_on_identity)
        );

        // 4. Switch configured agent to "override-off", reset global to true -> override false wins, notice omitted
        *config.lock().unwrap() = serde_json::json!({
            "memory": {
                "enabled": true,
                "agent": "override-off",
                "write_notice": { "enabled": true },
                "agents": {
                    "override-off": { "write_notice": { "enabled": false } },
                    "override-on": { "write_notice": { "enabled": true } }
                }
            }
        });
        let ctx3 = test_context("session-3", &project_dir, &agent_dir);
        for handler in api
            .registered
            .handlers
            .get(&maho_ext_api::EventKind::SessionStart)
            .expect("SessionStart handlers")
        {
            (handler)(
                &mut maho_ext_api::ExtensionEvent::SessionStart(maho_ext_api::SessionStartEvent {
                    reason: maho_ext_api::SessionReason::Startup,
                    initial_model_provenance: None,
                    previous_session_file: None,
                    attended: None,
                }),
                &ctx3,
            )
            .await
            .expect("session start dispatch");
        }
        runtime.capture_context(&ctx3);
        let override_off_identity = runtime
            .active_identity()
            .expect("bound active identity")
            .identity;
        assert!(
            override_off_identity.starts_with("override-off"),
            "session bound the override-off agent: {override_off_identity}"
        );

        let call4 = maho_ext_api::ToolCall {
            id: "c4",
            params: serde_json::json!({"command":"create","reason":"override off write","file_path":"reference/off.md","description":"off","file_text":"content4"}),
            signal: Default::default(),
            on_update: None,
            context: None,
        };
        let res4 = (tool.definition.execute)(call4)
            .await
            .expect("override-off create succeeds");
        let details4 = res4.details.expect("details present");
        assert!(
            details4.get("writeNotice").is_none(),
            "writeNotice omitted for override-off despite global true"
        );
    }
}
