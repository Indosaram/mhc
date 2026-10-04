//! Production host assembly for pinned omo-senpi memory `wiring.ts`.
//!
//! The mount owner constructs ONE `MemoryRuntime` per retained OMO mount, then
//! uses `options()` in its memory factory. `capture_context` must run before
//! each event/tool dispatch (including resumed RPC contexts). Configuration is
//! a live, resolved OMO root object, never a `{global, project}` snapshot.

use std::{collections::{BTreeMap, BTreeSet}, path::{Path, PathBuf}, sync::{Arc, Mutex, PoisonError}};

use maho_ext_api::{ExtensionActions, ExtensionContext, ExtensionUi, NotificationType};
use maho_omo_memory::{composition::MemoryExtensionOptions, context::MemoryIdentityContext,
    facts_runner::NativeFactsAttemptOptions, index::{MemoryComponent, MemoryComponentOptions},
    wiring::{MemoryWiring, create_memory_wiring}, wiring_runtime::MemoryRuntimeWiring,
    wiring_static::MemoryStaticOptions, wiring_types::{MemoryWiringOptions, NativeMemoryWiringOptions}, worker};
use memory_core::{git::GitMemoryRepo, reflection::{ReflectionReservationStore, ReservedRun}};
use serde_json::Value;

pub type LiveMemoryConfig = Arc<dyn Fn() -> Result<Value, String> + Send + Sync>;
pub type MemoryWarning = Arc<dyn Fn(&str) + Send + Sync>;

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
    pub which: Arc<dyn Fn(&str) -> Option<String> + Send + Sync>,
    pub warn: MemoryWarning,
}

struct IdentityWorker {
    identity: MemoryIdentityContext,
    store: Arc<ReflectionReservationStore>,
    runner: Mutex<worker::runner::SenpiSubprocessRunner>,
    cache: Mutex<worker::model_preflight::ModelPreflight>,
    sandbox: Mutex<maho_omo_memory::identity_runtime::IdentitySandbox>,
}

/// Retain this alongside the mount across extension reloads. Every resolver,
/// extractor, trigger and worker shares this component and this wiring object.
pub struct MemoryRuntime {
    pub component: Arc<MemoryComponent>,
    pub wiring: Arc<tokio::sync::Mutex<MemoryWiring>>,
    host: MemoryRuntimeHost,
    current: Mutex<Option<ExtensionContext>>,
    workers: Mutex<BTreeMap<String, Arc<IdentityWorker>>>,
    ledgers: Mutex<BTreeMap<String, Arc<Mutex<maho_omo_memory::context::MemoryPendingLedger>>>>,
    write_sessions: Mutex<BTreeMap<String, Arc<Mutex<maho_omo_memory::wiring_memory_write::MemoryWriteSession>>>>,
    nudge: Arc<Mutex<maho_omo_memory::nudge_wiring::MemoryNudgeWiring>>,
    skills: maho_omo_memory::skills_usage_wiring::SkillsUsageTrackers,
    health_notices: Mutex<BTreeSet<String>>,
    prompt: Arc<maho_omo_memory::prompt::MemoryPromptHandler>,
    advisory_notified: Mutex<BTreeSet<String>>,
}

impl MemoryRuntime {
    fn register_host(self: &Arc<Self>, api: &mut maho_ext_api::ExtensionApi) {
        maho_omo_memory::bindings::register_memory_binding_renderer(api);
        let this = self.clone();
        for kind in [maho_ext_api::EventKind::BeforeAgentStart, maho_ext_api::EventKind::SessionStart,
            maho_ext_api::EventKind::AgentSettled, maho_ext_api::EventKind::ToolCall] {
            let this = this.clone();
            api.on(kind, Arc::new(move |_, context| {
                this.capture_context(context);
                Box::pin(async { Ok(maho_ext_api::EventResult::None) })
            }));
        }
        let this = self.clone(); let resolve_session = Arc::new(move |context: &ExtensionContext| {
            this.capture_context(context);
            this.identity(context.session_manager.session_id()).map(|identity| (context.session_manager.session_id().into(), identity.identity))
        });
        let this = self.clone(); let resolve_active_session = Arc::new(move || this.context().map(|context| context.session_manager.session_id().to_owned()));
        let this = self.clone(); let resolve_settings = Arc::new(move |identity: &str| {
            this.settings().and_then(|settings| maho_omo_memory::dream_trigger_gates::resolve_dream_trigger_settings(&settings, Some(identity)))
                .unwrap_or_else(|error| { (this.host.warn)(&error); maho_omo_memory::dream_trigger_gates::DreamTriggerSettings { enabled: false, ..Default::default() } })
        });
        let this = self.clone(); let launch: maho_omo_memory::dream_trigger::DreamLaunch = Arc::new(move |session, origin, request, signal| {
            let this = this.clone(); Box::pin(async move {
                let identity = this.identity(&session).ok_or("no bound memory session")?;
                let worker = this.worker(&identity)?;
                let settings = this.settings()?;
                let policy = maho_omo_memory::dream_trigger_gates::resolve_dream_trigger_settings(&settings, Some(&identity.identity))?;
                let runtime = maho_omo_memory::identity_runtime::MemoryIdentityRuntime {
                    identity, store: worker.store.clone(), runner: Default::default(), sandbox: Default::default(),
                };
                let mut launch = |run| this.launch(worker.clone(), run);
                let mut session = maho_omo_memory::wiring_runtime::RuntimeDreamSession { session_id: session, runtime: &runtime, launch: &mut launch };
                maho_omo_memory::dream_trigger_fire::fire_dream(&mut session, origin, &policy, &request,
                    &|| memory_core::support::time::now_millis() as f64, &|| signal.as_ref().is_some_and(maho_ext_api::AbortSignal::is_aborted),
                    &mut |error| (this.host.warn)(&format!("memory dream launch failed: {error:?}"))).map_err(|error| format!("{error:?}"))
            })
        });
        let dream = Arc::new(maho_omo_memory::dream_trigger::DreamTriggerWiring::new(maho_omo_memory::dream_trigger::DreamTriggerOptions {
            resolve_session, resolve_active_session, resolve_settings, launch, warn: self.host.warn.clone(),
        }));
        dream.register(api);
        // Registration runs before async lifecycle dispatch; this wiring lock
        // cannot be held yet. The evaluator must survive subsequent shutdowns.
        match self.wiring.try_lock(){Ok(mut wiring)=>wiring.register_shutdown_evaluator(dream.shutdown_evaluator()),Err(error)=>std::panic::panic_any(format!("memory wiring locked during static registration: {error}"))}
        let this = self.clone();
        maho_omo_memory::commands::register::register_memory_commands(api, maho_omo_memory::commands::register::MemoryCommandDeps {
            resolve_context: {let this=this.clone();Arc::new(move |session| this.identity(session))},
            settings: {let this=this.clone();Arc::new(move || this.settings())},
            actions: self.host.actions.clone(),
            prompt: self.prompt.clone(),
            sessions_dir: self.host.agent_dir.join("sessions"),
            reflect: {let this=this.clone();Arc::new(move |session, event| {
                let identity=this.identity(session).ok_or("no bound memory session")?;
                let worker=this.worker(&identity)?;
                let result=worker.store.evaluate(session,event).map_err(|error|error.to_string())?.ok_or("reflection reservation rejected")?;
                if result.status=="active"{this.launch(worker,result.run.clone())?;}
                Ok((result.status,result.run.run_id))
            })}, dream,
            facts_retry: {let this=this.clone();Arc::new(move |identity| {
                let this=this.clone();Box::pin(async move {
                    let mut wiring=this.wiring.lock().await;
                    let facts=wiring.runtime.existing_facts_wiring(&identity).ok_or("facts extractor is not bound")?;
                    facts.reconcile_extractor();Ok(())
                })
            })},
        });
    }

    pub fn new(host: MemoryRuntimeHost) -> Result<Arc<Self>, String> {
        let initial = (host.load_config)()?;
        if initial.get("global").is_some() || initial.get("project").is_some() { return Err("memory builder requires live resolved OMO config, not a global/project envelope".into()); }
        maho_omo_memory::reflection_settings::resolve_memory_settings(initial.get("memory"))?;
        let component = MemoryComponent::new(MemoryComponentOptions {
            cwd: host.cwd.clone(), env: host.env.clone(), load_config: host.load_config.clone(),
            now: Arc::new(|| memory_core::support::time::now_millis() as f64), disabled: host.disabled.clone(),
        });
        let skills = Arc::new(Mutex::new(BTreeMap::new()));
        let wiring = Arc::new(tokio::sync::Mutex::new(create_memory_wiring(MemoryWiringOptions {
            runtime: MemoryRuntimeWiring::default(), skills_usage: skills.clone(),
        })));
        Ok(Arc::new(Self { component, wiring, host, skills, current: Mutex::new(None), workers: Mutex::new(BTreeMap::new()),
            ledgers: Mutex::new(BTreeMap::new()), write_sessions: Mutex::new(BTreeMap::new()),
            nudge: Arc::new(Mutex::new(Default::default())), health_notices: Mutex::new(BTreeSet::new()),
            prompt: Arc::new(Default::default()), advisory_notified: Mutex::new(BTreeSet::new()) }))
    }

    pub fn capture_context(&self, context: &ExtensionContext) {
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = Some(context.clone());
    }

    pub fn settings(&self) -> Result<Value, String> {
        let config = (self.host.load_config)()?;
        maho_omo_memory::reflection_settings::resolve_memory_settings(config.get("memory"))
    }

    fn context(&self) -> Option<ExtensionContext> {
        self.current.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn identity(&self, session: &str) -> Option<MemoryIdentityContext> {
        self.component.sessions.lock().unwrap_or_else(PoisonError::into_inner).get(session)
            .filter(|state| state.enabled).and_then(|state| state.context.clone())
    }

    fn active_identity(&self) -> Option<MemoryIdentityContext> {
        self.context().and_then(|context| self.identity(context.session_manager.session_id()))
    }

    fn worker(&self, identity: &MemoryIdentityContext) -> Result<Arc<IdentityWorker>, String> {
        let mut workers = self.workers.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(worker) = workers.get(&identity.identity) { return Ok(worker.clone()); }
        let store = maho_omo_memory::identity_runtime::create_identity_reservation_store(identity, &self.settings()?)?;
        let worker = Arc::new(IdentityWorker { identity: identity.clone(), store, runner: Mutex::new(Default::default()),
            cache: Mutex::new(Default::default()), sandbox: Mutex::new(Default::default()) });
        workers.insert(identity.identity.clone(), worker.clone());
        Ok(worker)
    }

    fn facts(self: &Arc<Self>, identity: &MemoryIdentityContext, context: &ExtensionContext)
        -> Result<maho_omo_memory::facts_wiring::MemoryFactsWiringOptions, String> {
        self.capture_context(context);
        let settings = self.settings()?;
        let people = &settings["people"];
        let this = self.clone();
        let registry = maho_omo_memory::model_registry_resolver::resolve_native_memory_model_registry(context);
        let sandbox_host = self.clone();
        let sandbox_identity = identity.identity.clone();
        let attempt = NativeFactsAttemptOptions {
            resolve_model: Arc::new(move || worker::resolve_model::resolve_reflection_model("quick", &(this.host.load_config)()?, Some(&registry), None).map_err(|error| error.to_string())),
            env: self.host.env.clone(), config_sources: (self.host.config_sources)(), launch: self.host.launcher.clone(),
            supervisor_command: self.host.supervisor_command.clone(), supervisor_args: self.host.supervisor_args.clone(),
            deadline_ms: 15 * 60_000, termination_grace_ms: 5_000, max_output_bytes: 1024 * 1024,
            people: memory_core::facts::person_routing::FactsPeopleRouting {
                enabled: people["enabled"].as_bool().ok_or("people.enabled missing")?,
                max_entries: people["max_entries"].as_u64().ok_or("people.max_entries missing")? as usize,
                max_entry_chars: people["max_entry_chars"].as_u64().ok_or("people.max_entry_chars missing")? as usize,
            },
            sandbox: Some(Arc::new(move |args| {
                let settings = sandbox_host.settings()?;
                let reflection = maho_omo_memory::reflection_settings::resolve_agent_reflection_settings(Some(&settings), &sandbox_identity)?;
                maho_omo_memory::sandbox::apply_facts_sandbox(args, maho_omo_memory::sandbox::FactsSpawnSandboxInput {
                    policy: sandbox_policy(&reflection)?, agent_dir: &sandbox_host.host.agent_dir, foreign_roots: &[], platform: platform(),
                }, sandbox_host.host.which.as_ref(), |message, _| (sandbox_host.host.warn)(message)).map_err(|error| error.to_string())
            })), warn: self.host.warn.clone(),
        };
        let this = self.clone();
        Ok(MemoryRuntimeWiring::native_facts_options(identity, Arc::new(move || this.settings()), attempt, Arc::new(memory_core::support::time::now_millis)))
    }

    /// The factory is repeatable: options share all retained state on reload.
    pub fn options(self: &Arc<Self>) -> MemoryExtensionOptions {
        let this = self.clone();
        let facts = Arc::new(move |identity: &MemoryIdentityContext, context: &ExtensionContext| this.facts(identity, context));
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
        MemoryExtensionOptions {
            component: self.component.clone(), wiring: self.wiring.clone(),
            wiring_options: NativeMemoryWiringOptions { facts, reconcile,
                now: Arc::new(|| memory_core::support::time::now_millis() as f64), warn: self.host.warn.clone() },
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
        let this = self.clone();
        let resolve: maho_omo_memory::prompt::PromptContextResolver = Arc::new(move |session| this.identity(session));
        let this = self.clone();
        let nudge = Arc::new(move |repo: &GitMemoryRepo, session: &str, identity: &str| {
            let settings = this.settings()?;
            let setting = &settings["nudge"]; let override_ = &settings["agents"][identity]["nudge"];
            this.nudge.lock().unwrap_or_else(PoisonError::into_inner).nudge_turns(repo, session,
                &maho_omo_memory::nudge_wiring::ResolvedNudgeSettings {
                    enabled: override_["enabled"].as_bool().or_else(|| setting["enabled"].as_bool()).ok_or("nudge.enabled missing")?,
                    every_user_turns: override_["every_user_turns"].as_i64().or_else(|| setting["every_user_turns"].as_i64()).ok_or("nudge threshold missing")?,
                }).map_err(|error| error.to_string())?.map(|turns|usize::try_from(turns).map_err(|error|error.to_string())).transpose()
        });
        let soul_context = resolve.clone();
        let soul = Arc::new(move |repo: &GitMemoryRepo, session: &str, _: &str| {
            let identity = soul_context(session).ok_or("no bound memory session")?;
            memory_core::soul::watermark::consume_soul_notice_delta(repo, &memory_core::soul::watermark::ConsumeSoulNoticeOptions {
                notices_dir: identity.identity_paths.notices, locks_dir: identity.identity_paths.locks, wait_timeout_ms: Some(2_000),
            }).map(|notice| notice.map(|notice| notice.sha)).map_err(|error| error.to_string())
        });
        let this = self.clone(); let search = Arc::new(move || match this.settings() {
            Ok(settings) => settings["tool_exposure"] == "search", Err(error) => { (this.host.warn)(&error); false }
        });
        let this = self.clone(); let tools = Arc::new(move || this.active_identity());
        let cwd = self.host.cwd.clone(); let resolve_cwd = Arc::new(move || cwd.clone());
        let this = self.clone(); let edit_notice = Arc::new(move |identity: &str| match this.settings() {
            Ok(settings) => settings["agents"][identity]["soul"]["edit_notice"].as_bool().or_else(|| settings["soul"]["edit_notice"].as_bool()).unwrap_or(true),
            Err(error) => { (this.host.warn)(&error); false }
        });
        let this = self.clone(); let writes = Arc::new(move |session: &str| {
            let identity = this.identity(session)?;
            Some(this.write_sessions.lock().unwrap_or_else(PoisonError::into_inner).entry(session.into()).or_insert_with(||
                Arc::new(Mutex::new(maho_omo_memory::wiring_memory_write::MemoryWriteSession { context: identity, memory_status_attempted: false }))).clone())
        });
        let this = self.clone(); let refresh = Arc::new(move |identity: &MemoryIdentityContext, context: &ExtensionContext| this.refresh_status(identity, context));
        let this = self.clone(); let on_write = Arc::new(move |session: &str| {
            let identity = this.identity(session).ok_or("no bound memory session")?;
            let context = this.context().ok_or("no captured memory context")?;
            this.refresh_status(&identity, &context).map(|_| ())
        });
        let this = self.clone(); let skills = Arc::new(move |_: &maho_ext_api::ToolCallEvent| this.active_identity());
        let this = self.clone(); let triggers = Arc::new(move |session: Option<&str>| {
            let session = session.map(str::to_owned).or_else(|| this.context().map(|context| context.session_manager.session_id().to_owned()))?;
            let identity = this.identity(&session)?;
            match this.trigger_session(&session, &identity) { Ok(session) => Some(session), Err(error) => { (this.host.warn)(&error); None } }
        });
        let this = self.clone(); let launch = Arc::new(move |request: memory_core::reflection::ReflectionRequest| {
            let identity = this.active_identity().ok_or("no bound memory session")?;
            let worker = this.worker(&identity)?;
            let active = memory_core::reflection::ReflectionReservationStore::read_state(&worker.store).map_err(|error| error.to_string())?.active.ok_or("no active reflection reservation")?;
            if active.request != request { return Err("reflection request does not match active reservation".into()); }
            this.launch(worker, active)
        });
        MemoryStaticOptions {
            register_host: Some({let this=self.clone();Arc::new(move |api| this.register_host(api))}),
            skills_trackers: Some(self.skills.clone()),
            prompt_handler: Some(self.prompt.clone()),
            prompt: maho_omo_memory::prompt::MemoryPromptInjectionOptions {
                resolve_context: resolve.clone(), create_repo: Some(Arc::new(|identity| GitMemoryRepo::open(&identity.identity_paths.repo, &identity.identity).map_err(|error| error.to_string()))),
                search_exposure: Some(search), resolve_nudge_turns: Some(nudge), resolve_soul_notice: Some(soul),
            }, nudge: self.nudge.clone(), resolve_context: resolve, resolve_tool_context: tools, resolve_cwd: resolve_cwd.clone(),
            captured_tools: self.host.captured_tools.clone(), warn: self.host.warn.clone(), edit_notice,
            theme: Arc::new(|theme| Arc::new(MemoryEntryTheme(theme.clone()))),
            memory_write: maho_omo_memory::wiring_memory_write::MemoryWriteOptions { resolve_session: writes, on_memory_write: on_write, refresh_status: refresh },
            skills_usage: maho_omo_memory::skills_usage_wiring::SkillsUsageOptions { resolve_context: skills, resolve_cwd, now_ms: Arc::new(|| memory_core::support::time::now_millis().max(0) as u64) },
            triggers: maho_omo_memory::trigger_wiring::ReflectionTriggerWiringOptions { resolve_session: triggers, on_launch: launch, warn: self.host.warn.clone() },
        }
    }

    fn trigger_session(&self, session: &str, identity: &MemoryIdentityContext) -> Result<maho_omo_memory::trigger_wiring::ReflectionTriggerSession, String> {
        let worker = self.worker(identity)?;
        let enabled = maho_omo_memory::trigger_wiring::resolve_reflection_trigger_config(&self.settings()?, Some(&identity.identity))?.enabled;
        let ledger = self.ledgers.lock().unwrap_or_else(PoisonError::into_inner).entry(session.into())
            .or_insert_with(|| Arc::new(Mutex::new(identity.ledger.clone()))).clone();
        Ok(maho_omo_memory::trigger_wiring::ReflectionTriggerSession { conversation_id: session.into(), ledger, engine: worker.store.clone(), enabled })
    }

    fn refresh_status(&self, identity: &MemoryIdentityContext, context: &ExtensionContext) -> Result<maho_omo_memory::status::MemoryStatusResult, String> {
        let settings = self.settings()?;
        let repo = GitMemoryRepo::open(&identity.identity_paths.repo, &identity.identity).map_err(|error| error.to_string())?;
        let key=format!("{}:{}",context.session_manager.session_id(),identity.identity);
        let result=maho_omo_memory::status::refresh_memory_status(&repo, &mut StatusUi(context.ui.clone()), &maho_omo_memory::status::RefreshMemoryStatusInput {
            context: identity, compile_warn_tokens: settings["compile_warn_tokens"].as_u64().ok_or("compile_warn_tokens missing")? as usize,
            already_notified: self.advisory_notified.lock().unwrap_or_else(PoisonError::into_inner).contains(&key), now_ms: memory_core::support::time::now_millis(), show_footer: context.has_ui,
            check_advisory: true, session_id: Some(context.session_manager.session_id()),
        }).map_err(|error| error.to_string())?;
        if result.notified{self.advisory_notified.lock().unwrap_or_else(PoisonError::into_inner).insert(key);}
        Ok(result)
    }

    fn launch(self: &Arc<Self>, worker: Arc<IdentityWorker>, run: ReservedRun) -> Result<(), String> {
        let this = self.clone();
        std::thread::Builder::new().name(format!("memory-{}", run.run_id)).spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())
                .and_then(|runtime| runtime.block_on(this.run_worker(&worker, run)));
            if let Err(error) = result { (this.host.warn)(&format!("memory reflection launch failed: {error}")); }
        }).map(|_| ()).map_err(|error| error.to_string())
    }

    async fn run_worker(self: &Arc<Self>, worker: &IdentityWorker, mut run: ReservedRun) -> Result<(), String> {
        loop {
            let context = self.context().ok_or("no captured memory context")?;
            let config = (self.host.load_config)()?;
            let settings = self.settings()?;
            let reflection = maho_omo_memory::reflection_settings::resolve_agent_reflection_settings(Some(&settings), &worker.identity.identity)?;
            let registry = maho_omo_memory::model_registry_resolver::resolve_native_memory_model_registry(&context);
            let session_model = context.model.as_ref().map(|model| worker::resolve_model::ReflectionSessionModel { provider: model.provider.clone(), id: model.id.clone(), thinking: None });
            let category = reflection["category"].as_str().ok_or("reflection.category missing")?;
            let resolution = worker::resolve_model::resolve_reflection_model(category, &config, Some(&registry), session_model.as_ref()).map_err(|error| error.to_string())?;
            let route = if matches!(resolution, worker::resolve_model::ReflectionModelResolution::Resolved { .. }) {
                Some(worker::runner::SenpiSubprocessRunner::choose_launch_route(&run, &resolution, Some(&registry), session_model.as_ref(),
                    maho_omo_memory::session_context_resolver::resolve_native_parent_context_tokens(&context).map_err(|error| error.to_string())?, (self.host.parent_cache_reusable)(&context))?)
            } else { None };
            let identity = maho_omo_memory::identity_runtime::as_memory_identity(&worker.identity);
            let now = memory_core::support::time::now_millis();
            let started = memory_core::support::time::format_rfc3339_millis(now);
            let sources = (self.host.config_sources)();
            let sandbox = |args| worker.sandbox.lock().unwrap_or_else(PoisonError::into_inner).apply(
                maho_omo_memory::identity_runtime::IdentitySandboxInput { identity: &worker.identity, policy: sandbox_policy(&reflection)?, agent_dir: &self.host.agent_dir, platform: platform() },
                args, self.host.which.as_ref(), |message| (self.host.warn)(message));
            let ensure_renderer = || (self.host.ensure_completion_renderer)();
            let health_alert = |dir: &Path| {
                let mut live = LiveSession { runtime: self, context: &context, identity: &identity.id };
                worker::health_alert::emit_reflection_health_alert(dir, &identity.id, Some(&mut live), &mut |key|
                    self.health_notices.lock().unwrap_or_else(PoisonError::into_inner).insert(key.into()), memory_core::support::time::now_millis());
            };
            let append_launched = || {
                let mut live = LiveSession { runtime: self, context: &context, identity: &identity.id };
                worker::runner::SenpiSubprocessRunner::append_launched(&run, &identity, &resolution, &started, Some(&mut live), |conversation|
                    memory_core::journal::store::TranscriptJournal::new(memory_core::journal::store::TranscriptJournalOptions::new(identity.paths.transcripts.join(conversation))).get_state().map(Some).map_err(|error| error.to_string()))
            };
            let mut live = LiveSession { runtime: self, context: &context, identity: &identity.id };
            let mut runner = worker.runner.lock().unwrap_or_else(PoisonError::into_inner);
            let mut cache = worker.cache.lock().unwrap_or_else(PoisonError::into_inner);
            let dir = identity.paths.reflection.join("runs").join(&run.run_id);
            let result = runner.launch_native(worker::runner::ReflectionRunnerInput {
                run: &run, identity: &identity, config: &config, resolution: &resolution, reservation: worker.store.as_ref(), started_at: &started, now_ms: &memory_core::support::time::now_millis,
            }, &mut cache, worker::runner::NativeReflectionExecutionOptions {
                env: &self.host.env, sources: &sources, launcher: &self.host.launcher, parent_session_file: context.session_manager.session_file(), parent_cwd: Some(&self.host.cwd), route: route.as_ref(),
                sandbox: Some(&sandbox), deadline_ms: None,
                child: worker::spawn_supervisor::ReflectionChildOptions { termination_grace_ms: 5_000.0, max_output_bytes: 1024 * 1024, supervisor_command: &self.host.supervisor_command, supervisor_args: &self.host.supervisor_args, launched_at: now },
                callbacks: worker::runner::NativeReflectionCallbacks { ensure_renderer: &ensure_renderer, health_alert: &health_alert, append_launched: &append_launched, warn: self.host.warn.as_ref() },
            }, Some(&mut live), |operation| terminal_gate(&dir, &run.run_id, operation)).await?;
            drop(cache); drop(runner);
            match result.launch { Some(next) => run = next, None => return Ok(()) }
        }
    }

    async fn reconcile_worker(self: &Arc<Self>, worker: &Arc<IdentityWorker>) -> Result<(), String> {
        let identity = maho_omo_memory::identity_runtime::as_memory_identity(&worker.identity);
        let launch = |run: &ReservedRun| if let Err(error) = self.launch(worker.clone(), run.clone()) { (self.host.warn)(&error); };
        let context = worker::run_finalization_types::RunFinalizationContext {
            identity: &identity, reservation: worker.store.as_ref(), launch: Some(&launch), now_ms: &memory_core::support::time::now_millis,
        };
        let gate = |dir: &Path, run: &str, operation: &mut dyn FnMut() -> Result<Option<worker::run_finalization_types::ReservationRunResult>, String>| terminal_gate(dir, run, operation);
        let recovery = worker::run_reconciliation::NativeReflectionRecovery { context: &context, terminal_gate: &gate };
        worker::run_reconciliation::reconcile_reflection_runs(&context, &recovery).await.map(|_| ())
    }
}

fn terminal_gate<T>(dir: &Path, run: &str, operation: &mut dyn FnMut() -> Result<T, String>) -> Result<T, String> {
    worker::run_terminal_gate::with_run_terminal_gate(dir, run, operation, |path, record, operation| {
        // Source uses an unbounded native lock, not a bounded failure fallback.
        loop {
            match memory_core::locks::acquire_lock(path, record, &memory_core::locks::AcquireLockOptions { wait_timeout_ms: Some(60_000), ..Default::default() }) {
                Ok(()) => break,
                Err(memory_core::locks::AcquireLockError::Contention(_)) => continue,
                Err(error) => return Err(error.to_string()),
            }
        }
        let result = operation();
        memory_core::locks::release_lock(path, record).map_err(|error| error.to_string())?;
        result
    })
}

fn sandbox_policy(settings: &Value) -> Result<maho_omo_memory::sandbox::SandboxPolicy, String> {
    match settings["sandbox"].as_str() {
        Some("required") => Ok(maho_omo_memory::sandbox::SandboxPolicy::Required),
        Some("auto") => Ok(maho_omo_memory::sandbox::SandboxPolicy::Auto),
        Some("off") => Ok(maho_omo_memory::sandbox::SandboxPolicy::Off),
        _ => Err("reflection.sandbox missing or invalid".into()),
    }
}
fn platform() -> &'static str { if cfg!(target_os = "macos") { "darwin" } else if cfg!(target_os = "windows") { "win32" } else { "linux" } }

struct StatusUi(Arc<dyn ExtensionUi>);
impl maho_omo_memory::status::MemoryStatusUi for StatusUi {
    fn set_status(&mut self, key: &str, text: Option<&str>) { self.0.set_status(key, text); }
    fn notify(&mut self, message: &str, _: &str) { self.0.notify(message, NotificationType::Warning); }
}
struct MemoryEntryTheme(maho_ext_api::Theme);
impl worker::entry_renderers::EntryRenderTheme for MemoryEntryTheme {
    fn fg(&self, tone: &str, text: &str) -> String {
        let color = self.0.colors.get(tone).map(String::as_str).unwrap_or("");
        let prefix = color.strip_prefix('#').filter(|hex| hex.len() == 6).and_then(|hex| u32::from_str_radix(hex, 16).ok())
            .map(|rgb| format!("\x1b[38;2;{};{};{}m", (rgb >> 16) & 255, (rgb >> 8) & 255, rgb & 255)).unwrap_or_else(|| "\x1b[39m".into());
        format!("{prefix}{text}\x1b[39m")
    }
    fn italic(&self, text: &str) -> String { format!("\x1b[3m{text}\x1b[23m") }
}
struct LiveSession<'a> { runtime: &'a MemoryRuntime, context: &'a ExtensionContext, identity: &'a str }
impl worker::completion_delivery::ReflectionLiveSession for LiveSession<'_> {
    fn session_id(&self) -> &str { self.context.session_manager.session_id() }
    fn append_entry(&mut self, kind: &str, data: Value) {
        if let Err(error) = self.runtime.host.actions.append_entry(kind, Some(data)) { (self.runtime.host.warn)(&error.to_string()); }
    }
    fn notify(&mut self, message: &str, warning: bool) -> Result<(), String> {
        self.context.ui.notify(message, if warning { NotificationType::Warning } else { NotificationType::Info }); Ok(())
    }
    fn warn(&mut self, message: &str, error: &str) { (self.runtime.host.warn)(&format!("{message}: {error}")); }
    fn on_completion(&mut self, _: &str) {
        if let Some(identity) = self.runtime.identity(self.context.session_manager.session_id()).filter(|identity| identity.identity == self.identity)
            && let Err(error) = self.runtime.refresh_status(&identity, self.context) { (self.runtime.host.warn)(&error); }
    }
}
impl worker::health_alert::ReflectionHealthLiveSession for LiveSession<'_> {
    fn session_id(&self) -> &str { self.context.session_manager.session_id() }
    fn has_ui(&self) -> bool { self.context.has_ui }
    fn append_entry(&mut self, kind: &str, entry: &worker::health_alert::ReflectionHealthEntry) {
        match serde_json::to_value(entry) {
            Ok(data) => worker::completion_delivery::ReflectionLiveSession::append_entry(self, kind, data),
            Err(error) => (self.runtime.host.warn)(&error.to_string()),
        }
    }
    fn notify(&mut self, message: &str, _: &str) { self.context.ui.notify(message, NotificationType::Warning); }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoopActions;
    impl ExtensionActions for NoopActions {
        fn send_message(&self, _: maho_ext_api::CustomMessage, _: maho_ext_api::SendMessageOptions) -> Result<(), maho_ext_api::ExtensionFailure> { Ok(()) }
        fn send_user_message(&self, _: maho_ext_api::UserMessageContent, _: maho_ext_api::SendUserMessageOptions) -> Result<(), maho_ext_api::ExtensionFailure> { Ok(()) }
        fn append_entry(&self, _: &str, _: Option<Value>) -> Result<(), maho_ext_api::ExtensionFailure> { Ok(()) }
        fn get_all_tools(&self) -> Result<Vec<maho_ext_api::ToolInfo>, maho_ext_api::ExtensionFailure> { Ok(Vec::new()) }
    }

    /// A host whose only live input is the shared config cell; nothing launches.
    fn host(config: Arc<Mutex<Value>>) -> MemoryRuntimeHost {
        let load: LiveMemoryConfig = { let config = config.clone(); Arc::new(move || Ok(config.lock().unwrap_or_else(PoisonError::into_inner).clone())) };
        MemoryRuntimeHost {
            cwd: PathBuf::from("/project"), agent_dir: PathBuf::from("/agent"), env: BTreeMap::new(),
            load_config: load,
            config_sources: Arc::new(Vec::new),
            launcher: worker::model_preflight::Launcher { command: "unused".into(), prefix_args: vec![] },
            supervisor_command: PathBuf::from("unused"), supervisor_args: vec![],
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
        assert!(MemoryRuntime::new(host(Arc::new(Mutex::new(serde_json::json!({ "global": {} }))))).is_err());
        assert!(MemoryRuntime::new(host(Arc::new(Mutex::new(serde_json::json!({ "project": {} }))))).is_err());
    }

    #[test]
    fn reads_live_resolved_settings_not_a_frozen_snapshot() {
        let config = Arc::new(Mutex::new(serde_json::json!({ "memory": { "soul": { "edit_notice": false } } })));
        let runtime = MemoryRuntime::new(host(config.clone())).unwrap();
        assert_eq!(runtime.settings().unwrap()["soul"]["edit_notice"], serde_json::json!(false));
        *config.lock().unwrap_or_else(PoisonError::into_inner) = serde_json::json!({ "memory": { "soul": { "edit_notice": true } } });
        assert_eq!(runtime.settings().unwrap()["soul"]["edit_notice"], serde_json::json!(true));
    }

    #[test]
    fn options_reuse_one_component_wiring_and_retained_ports() {
        let runtime = runtime(serde_json::json!({ "memory": {} }));
        let first = runtime.options();
        let second = runtime.options();
        assert!(Arc::ptr_eq(&first.component, &second.component));
        assert!(Arc::ptr_eq(&first.wiring, &second.wiring));
        let (first, second) = (first.static_options, second.static_options);
        assert!(Arc::ptr_eq(first.register_host.as_ref().unwrap(), second.register_host.as_ref().unwrap()));
        assert!(Arc::ptr_eq(first.skills_trackers.as_ref().unwrap(), second.skills_trackers.as_ref().unwrap()));
        assert!(Arc::ptr_eq(first.prompt_handler.as_ref().unwrap(), second.prompt_handler.as_ref().unwrap()));
    }

    #[test]
    fn component_is_the_retained_memory_list_entry() {
        let runtime = runtime(serde_json::json!({ "memory": {} }));
        assert_eq!(runtime.component().name, "memory");
    }
}
