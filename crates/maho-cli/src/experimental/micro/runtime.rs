//! Port of senpi `experimental/micro/runtime.ts` (target 59d58cee): `openMicro` wires one micro
//! session (a lock-gated Pico3 `JsonlStorage`), the micro tools and models, and exposes the plain
//! data `MicroView` plus a `MicroController`.
//!
//! Pico3 here is `Rc`-based, so the whole runtime is single-threaded: callers drive it inside a
//! `tokio::task::LocalSet` (Pico3's scheduler already spawns local tasks). Differences from senpi, each forced by the Rust API:
//! - senpi's `queueMicrotask(drain)` batching becomes a synchronous fold in the watch listener; the
//!   listener already runs after the commit settles, so the observable view sequence is the same.
//! - The lost-lock callback fires on the lock heartbeat thread. It only sets a flag and wakes a
//!   local task (`tokio::sync::Notify`), which runs senpi's `stopOnLostLock` on the runtime thread.
//! - `harness.suspend` is awaited by that task; a failure goes to the debug log (names only), as
//!   senpi's `logLostLockStopFailure` does.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use maho_agent::harness::context::BACKGROUND_CONTEXT;
use maho_agent::harness::env::nodejs::NodeExecutionEnv;
use maho_agent::harness::pico3::harness::{ConversationHandle, Harness, HarnessOptions};
use maho_agent::harness::pico3::jsonl::JsonlStorage;
use maho_agent::harness::pico3::kinds::generation::GenerationHooks;
use maho_agent::harness::pico3::system::{HookOutput, SystemInstructionsHandler, system_sections};
use maho_agent::harness::pico3::types::{
    CompactionReason, CompactionView, ConversationView, CoreViewEvent, Entry, EntryScan, Envelope,
    Error, GenerationFailure, JsonObject, JsonValue, LocalBoxFuture, Message, ModelRef,
    NamespaceDefaults, SendInput, Storage, ViewEvent, WhenBusy,
};
use maho_agent::harness::pico3::view::{Watch, apply_envelope};
use maho_agent::harness::types::FileSystem;
use maho_ai::auth::types::{AuthEvent, AuthInteraction, AuthPrompt, AuthType};
use maho_ai::model::Model;
use maho_ai::models::{ModelsRefreshOptions, clamp_thinking_level, get_supported_thinking_levels};
use maho_ai::types::{AssistantMessage, ModelThinkingLevel, StopReason, Usage};
use maho_ai::utils::abort::{AbortController, AbortReason, AbortSignal};
use maho_core::compaction_settings_resolver::ResolvedCompactionSettings;
use maho_core::file_lock::{FileLockLostError, HeldFileLock, OnLostCallback};
use maho_core::model_resolver::{InitialModelOptions, find_initial_model};
use maho_core::model_runtime::ModelRuntime;
use tokio::sync::oneshot;

use super::api::{
    AuthNotice, AuthPromptRequest, MicroAuthPrompt, MicroAuthType, MicroAuthView, MicroController,
    MicroNotice, MicroNoticeLevel, MicroSessionView, MicroUsageView, MicroView, MicroViewSource,
};
use super::lock_gated_storage::lock_gated_storage;
use super::models::{create_pico_models, read_models_view};
use super::sessions::{MicroSessionLocation, select_session_in};
use super::tools::create_micro_tools;

const DEFAULT_MODEL: (&str, &str) = ("chatgpt-subscription", "gpt-5.6-sol");

pub const SYSTEM_PROMPT: &str = "You are an expert coding assistant working in a terminal.\nUse read to inspect files, bash to run commands, edit for precise changes, and write for new files or complete rewrites.\nKeep answers concise and technical.";

pub const LOST_LOCK_MESSAGE: &str =
    "Another process took over this micro session; it was stopped to protect its history.";

const MODEL_REFRESH_TIMEOUT_MS: u64 = 15_000;

/// senpi `OpenMicroOptions`.
#[derive(Clone, Debug, Default)]
pub struct OpenMicroOptions {
    pub cwd: Option<PathBuf>,
    pub continue_session: bool,
}

/// Seams the tests replace: the agent dir and the model runtime factory.
pub struct OpenMicroDeps {
    pub agent_dir: PathBuf,
    pub model_runtime: ModelRuntime,
    /// Called with the opened harness before `resume` (senpi tests spy on `Harness.open`).
    pub on_harness: Option<OnHarness>,
    /// Where `(header, detail)` debug entries go (senpi `appendDebugLogEntry`, which the tests
    /// mock); `None` is the brand debug log.
    pub debug_log: Option<DebugLog>,
}

/// senpi `appendDebugLogEntry(header, detail)`.
pub type DebugLog = Rc<dyn Fn(&str, &str)>;
pub type OnHarness = Box<dyn FnOnce(&Rc<Harness>)>;
type CompactionSettingsFn = Rc<dyn Fn(&Model) -> Option<ResolvedCompactionSettings>>;
type Listener = Rc<dyn Fn()>;

/// senpi `OpenMicroResult`.
pub struct OpenMicroResult {
    pub view: Rc<MicroRuntime>,
    pub controller: Rc<MicroRuntime>,
}

impl OpenMicroResult {
    /// senpi `close()`.
    pub async fn close(&self) {
        self.view.close().await;
    }
}

/// senpi `openMicro(options)` with production dependencies.
pub async fn open_micro(options: OpenMicroOptions) -> Result<OpenMicroResult, String> {
    crate::cli::setup::register_builtin_apis();
    let agent_dir = maho_core::config::get_agent_dir();
    let model_runtime = ModelRuntime::create(
        crate::experimental::mini::worker::model_runtime_options(&agent_dir),
    )
    .await
    .map_err(|error| error.to_string())?;
    open_micro_with(
        options,
        OpenMicroDeps {
            agent_dir: PathBuf::from(agent_dir),
            model_runtime,
            on_harness: None,
            debug_log: None,
        },
    )
    .await
}

struct PendingAuth {
    id: String,
    sender: oneshot::Sender<Result<String, String>>,
}

/// The shared runtime state behind both `MicroViewSource` and `MicroController`.
pub struct MicroRuntime {
    state: RefCell<MicroView>,
    listeners: RefCell<Vec<(u64, Listener)>>,
    next_listener: Cell<u64>,
    next_notice: Cell<u64>,
    closed: Cell<bool>,
    harness: Rc<Harness>,
    root: ConversationHandle,
    watch: RefCell<Option<Watch>>,
    restarting_watch: Cell<bool>,
    cumulative: RefCell<UsageAccumulator>,
    model_runtime: ModelRuntime,
    compaction: CompactionSettingsFn,
    env: Arc<NodeExecutionEnv>,
    location: RefCell<MicroSessionLocation>,
    refresh_controllers: RefCell<Vec<AbortController>>,
    login_controller: RefCell<Option<AbortController>>,
    pending_auth: RefCell<Option<PendingAuth>>,
    debug_log: DebugLog,
    myself: std::rc::Weak<MicroRuntime>,
}

/// senpi `openMicro` with injected dependencies.
pub async fn open_micro_with(
    options: OpenMicroOptions,
    deps: OpenMicroDeps,
) -> Result<OpenMicroResult, String> {
    let OpenMicroDeps {
        agent_dir,
        model_runtime,
        on_harness,
        debug_log,
    } = deps;
    let debug_log = debug_log.unwrap_or_else(|| {
        Rc::new(|header: &str, detail: &str| {
            maho_core::hidden_stdout_log::append_debug_log_entry(header, detail)
        })
    });
    // `stopOnLostLock` runs on this thread; the lock heartbeat only flags the loss and wakes it.
    let lost = Arc::new(AtomicBool::new(false));
    let wake = Arc::new(tokio::sync::Notify::new());
    let on_lost: OnLostCallback = {
        let (lost, wake) = (lost.clone(), wake.clone());
        Some(Arc::new(move |_error: &FileLockLostError| {
            lost.store(true, Ordering::SeqCst);
            wake.notify_one();
        }))
    };
    let cwd_input = match options.cwd {
        Some(cwd) => cwd,
        None => std::env::current_dir().map_err(|error| error.to_string())?,
    };
    let mut location =
        select_session_in(&agent_dir, &cwd_input, options.continue_session, on_lost)?;
    let env = Arc::new(NodeExecutionEnv::new(location.cwd.to_string_lossy()));
    let mut storage: Option<Rc<JsonlStorage>> = None;
    let opened = open_inner(
        &agent_dir,
        &location,
        &mut storage,
        &env,
        model_runtime,
        on_harness,
    )
    .await;
    match opened {
        Ok(parts) => {
            let runtime = finish_open(parts, location, env, debug_log);
            if let Err(error) = runtime.harness.resume(&BACKGROUND_CONTEXT).await {
                runtime.fail(error.to_string(), matches!(error, Error::Faulted(_)));
            }
            spawn_lost_lock_watch(&runtime, lost, wake);
            Ok(OpenMicroResult {
                view: runtime.clone(),
                controller: runtime,
            })
        }
        Err(error) => {
            if let Some(storage) = storage {
                let _ = storage.close(&BACKGROUND_CONTEXT).await;
            }
            FileSystem::cleanup(env.as_ref(), &BACKGROUND_CONTEXT).await;
            location.release();
            Err(error)
        }
    }
}

struct OpenParts {
    harness: Rc<Harness>,
    root: ConversationHandle,
    watch: Watch,
    cumulative: UsageAccumulator,
    model_runtime: ModelRuntime,
    compaction: CompactionSettingsFn,
    pending_reports: Rc<RefCell<Option<Vec<String>>>>,
    report_target: Rc<RefCell<std::rc::Weak<MicroRuntime>>>,
}

/// senpi `SettingsManager.create(cwd).getCompactionSettings(model)`, read once at open.
fn compaction_settings(
    cwd: &Path,
    agent_dir: &Path,
) -> CompactionSettingsFn {
    let settings = maho_core::settings_manager::SettingsManager::create(
        &cwd.to_string_lossy(),
        &agent_dir.to_string_lossy(),
        &maho_core::config::home_dir(),
        true,
    );
    let configured = settings.get_value("compaction").cloned().and_then(|value| {
        serde_json::from_value::<maho_core::compaction_settings_access::CompactionSettings>(value)
            .ok()
    });
    Rc::new(move |model: &Model| {
        maho_core::compaction_settings_resolver::resolve_compaction_settings(
            configured.as_ref(),
            Some(
                maho_core::compaction_settings_access::CompactionModelSelector {
                    provider: &model.provider,
                    id: &model.id,
                },
            ),
        )
        .ok()
    })
}

fn json(value: serde_json::Value) -> JsonValue {
    JsonValue::from(value)
}

fn object(value: serde_json::Value) -> JsonObject {
    match json(value) {
        JsonValue::Object(map) => JsonObject::from((*map).clone()),
        _ => JsonObject::new(),
    }
}

async fn open_inner(
    agent_dir: &Path,
    location: &MicroSessionLocation,
    storage_slot: &mut Option<Rc<JsonlStorage>>,
    env: &Arc<NodeExecutionEnv>,
    model_runtime: ModelRuntime,
    on_harness: Option<OnHarness>,
) -> Result<OpenParts, String> {
    let ctx = &*BACKGROUND_CONTEXT;
    let compaction = compaction_settings(&location.cwd, agent_dir);
    let micro_tools = create_micro_tools(env.clone())?;
    let pico_models = create_pico_models(
        model_runtime.clone(),
        micro_tools.model_metadata.clone(),
        location.cwd.to_string_lossy(),
    );
    let selected_tools: Vec<String> = micro_tools
        .declarations
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect();
    let preferred = model_runtime.get_model(DEFAULT_MODEL.0, DEFAULT_MODEL.1);
    let initial: Option<(Model, ModelThinkingLevel)> = if location.created {
        match preferred {
            Some(model) => Some((model, ModelThinkingLevel::Medium)),
            None => {
                let found = find_initial_model(
                    InitialModelOptions {
                        cli_provider: None,
                        cli_model: None,
                        scoped_models: &[],
                        is_continuing: false,
                        default_provider: None,
                        default_model_id: None,
                        model_thinking_levels: None,
                    },
                    &model_runtime,
                )
                .await?;
                found.parsed.model.map(|model| {
                    (
                        model,
                        found
                            .parsed
                            .thinking_level
                            .unwrap_or(ModelThinkingLevel::Off),
                    )
                })
            }
        }
    } else {
        None
    };
    let resolved = initial.as_ref().and_then(|(model, _)| compaction(model));
    let threshold = match (&initial, &resolved) {
        (Some((model, _)), Some(settings)) if settings.enabled => {
            (model.context_window as i64 - settings.reserve_tokens).max(0)
        }
        _ => 0,
    };

    let storage = Rc::new(JsonlStorage::open(&location.path, true).map_err(|e| e.to_string())?);
    *storage_slot = Some(storage.clone());
    let held = location.held_lock();
    let gated = lock_gated_storage(
        storage.clone() as Rc<dyn Storage>,
        Rc::new(move || held.as_ref().map_or(Ok(()), |held| held.assert_held())),
    );

    let mut rewindable = serde_json::json!({
        "selectedTools": selected_tools,
        "threshold": threshold,
        "keepRecent": resolved.as_ref().map_or(20_000, |settings| settings.keep_recent_tokens),
    });
    if let Some((model, thinking)) = &initial {
        rewindable["model"] = serde_json::json!({"provider": model.provider, "modelId": model.id});
        rewindable["thinkingLevel"] = serde_json::to_value(thinking).unwrap_or_default();
    }
    // senpi buffers reports until the runtime exists, then routes them to `fail`.
    let pending_reports: Rc<RefCell<Option<Vec<String>>>> = Rc::new(RefCell::new(Some(Vec::new())));
    let report_target: Rc<RefCell<std::rc::Weak<MicroRuntime>>> =
        Rc::new(RefCell::new(std::rc::Weak::new()));
    let mut harness_options = HarnessOptions::new(Rc::new(pico_models));
    harness_options.tools = micro_tools.declarations;
    harness_options.root.rewindable = Some(object(rewindable));
    harness_options.on_report = {
        let (pending, target) = (pending_reports.clone(), report_target.clone());
        Rc::new(move |error: &Error| {
            if let Some(buffer) = pending.borrow_mut().as_mut() {
                buffer.push(error.to_string());
                return;
            }
            if let Some(runtime) = target.borrow().upgrade() {
                runtime.report(error.to_string(), matches!(error, Error::Faulted(_)));
            }
        })
    };
    let harness = Harness::open(Rc::new(gated), harness_options, ctx)
        .await
        .map_err(|error| error.to_string())?;
    let root = harness.root(ctx).await.map_err(|error| error.to_string())?;
    let config = root
        .config_get(ctx)
        .await
        .map_err(|error| error.to_string())?;
    let wanted = json(serde_json::json!(selected_tools));
    if config
        .get("selectedTools")
        .map(|v| serde_json::to_string(v).ok())
        != Some(serde_json::to_string(&wanted).ok())
    {
        root.config_set(
            object(serde_json::json!({"selectedTools": selected_tools})),
            ctx,
        )
        .await
        .map_err(|error| error.to_string())?;
    }

    let namespace = harness
        .namespace("micro.system", NamespaceDefaults::default(), None)
        .map_err(|error| error.to_string())?;
    let kinds = harness.kinds().map_err(|error| error.to_string())?;
    let cwd = location.cwd.to_string_lossy().into_owned();
    let sections = system_sections();
    let handler: SystemInstructionsHandler = Rc::new(move |draft, _, _, _, _| {
        if let Some(identity) = sections.get("identity") {
            draft.set(identity, JsonValue::String(SYSTEM_PROMPT.into()));
        }
        if let Some(environment) = sections.get("environment") {
            draft.set(environment, json(serde_json::json!({"cwd": cwd})));
        }
        Box::pin(async { Ok::<Option<HookOutput>, Error>(None) })
    });
    // Registered for the harness's lifetime; the unregister handle is intentionally not called.
    let _unregister = harness
        .hooks(
            &namespace,
            &kinds.generation,
            Rc::new(GenerationHooks {
                system_instructions: Some(handler),
                ..GenerationHooks::default()
            }),
            None,
            false,
        )
        .map_err(|error| error.to_string())?;
    if let Some(on_harness) = on_harness {
        on_harness(&harness);
    }

    let watch = root.watch(ctx).await.map_err(|error| error.to_string())?;
    let cumulative = read_usage_accumulator(&harness, root.id).await?;
    Ok(OpenParts {
        harness,
        root,
        watch,
        cumulative,
        model_runtime,
        compaction,
        pending_reports,
        report_target,
    })
}

fn finish_open(
    parts: OpenParts,
    location: MicroSessionLocation,
    env: Arc<NodeExecutionEnv>,
    debug_log: DebugLog,
) -> Rc<MicroRuntime> {
    let OpenParts {
        harness,
        root,
        watch,
        cumulative,
        model_runtime,
        compaction,
        pending_reports,
        report_target,
    } = parts;
    let conversation = watch.view().clone();
    let usage = usage_view(&cumulative, &conversation, &model_runtime);
    let state = MicroView {
        session: MicroSessionView {
            id: location.id.clone(),
            path: location.path.to_string_lossy().into_owned(),
            cwd: location.cwd.to_string_lossy().into_owned(),
        },
        conversation,
        models: read_models_view(&model_runtime, false),
        usage,
        auth: None,
        notices: Vec::new(),
        fatal: None,
    };
    let runtime = Rc::new_cyclic(|myself| MicroRuntime {
        state: RefCell::new(state),
        listeners: RefCell::new(Vec::new()),
        next_listener: Cell::new(1),
        next_notice: Cell::new(1),
        closed: Cell::new(false),
        harness,
        root,
        watch: RefCell::new(Some(watch)),
        restarting_watch: Cell::new(false),
        cumulative: RefCell::new(cumulative),
        model_runtime,
        compaction,
        env,
        location: RefCell::new(location),
        refresh_controllers: RefCell::new(Vec::new()),
        login_controller: RefCell::new(None),
        pending_auth: RefCell::new(None),
        debug_log,
        myself: myself.clone(),
    });
    runtime.attach_watch();
    *report_target.borrow_mut() = Rc::downgrade(&runtime);
    let pending = pending_reports.borrow_mut().take().unwrap_or_default();
    for error in pending {
        runtime.report(error, false);
    }
    let configured = model_ref(runtime.state.borrow().conversation.config.get("model"));
    if let Some(configured) = configured
        && runtime
            .model_runtime
            .get_model(&configured.provider, &configured.model_id)
            .is_none()
    {
        runtime.notice(
            MicroNoticeLevel::Warning,
            format!(
                "Saved model is unavailable: {}/{}",
                configured.provider, configured.model_id
            ),
        );
    }
    runtime
}

fn spawn_lost_lock_watch(
    runtime: &Rc<MicroRuntime>,
    lost: Arc<AtomicBool>,
    wake: Arc<tokio::sync::Notify>,
) {
    let weak = Rc::downgrade(runtime);
    tokio::task::spawn_local(async move {
        while !lost.load(Ordering::SeqCst) {
            wake.notified().await;
        }
        if let Some(runtime) = weak.upgrade() {
            runtime.stop_on_lost_lock().await;
        }
    });
}

impl MicroRuntime {
    /// The session lock. Another process reclaiming it reports through this handle.
    pub fn session_lock(&self) -> Option<HeldFileLock> {
        self.location.borrow().held_lock()
    }

    fn publish(&self) {
        let listeners: Vec<_> = self
            .listeners
            .borrow()
            .iter()
            .map(|(_, listener)| listener.clone())
            .collect();
        for listener in listeners {
            listener();
        }
    }

    fn update(&self, patch: impl FnOnce(&mut MicroView)) {
        patch(&mut self.state.borrow_mut());
        self.publish();
    }

    fn notice(&self, level: MicroNoticeLevel, message: String) {
        let id = self.next_notice.replace(self.next_notice.get() + 1);
        self.update(|state| {
            state.notices.push(MicroNotice { id, level, message });
            let excess = state.notices.len().saturating_sub(50);
            state.notices.drain(..excess);
        });
    }

    fn fail(&self, message: String, faulted: bool) {
        if faulted {
            let fatal = message.clone();
            self.update(|state| state.fatal = Some(fatal));
        }
        self.notice(MicroNoticeLevel::Error, message);
    }

    fn report(&self, message: String, faulted: bool) {
        self.fail(message, faulted);
        if self.watch.borrow().as_ref().is_some_and(Watch::closed) {
            self.spawn_restart_watch();
        }
    }

    fn fold_event(&self, event: &ViewEvent, previous: Option<&CompactionView>) {
        let ViewEvent::Core(event) = event else {
            return;
        };
        match event {
            CoreViewEvent::EntryAdded { entry } => {
                accumulate_usage(&mut self.cumulative.borrow_mut(), entry)
            }
            CoreViewEvent::Warning { message, .. } => {
                self.notice(MicroNoticeLevel::Warning, message.clone())
            }
            CoreViewEvent::GenerationFailed { reason, detail, .. } => {
                let level = if matches!(reason, GenerationFailure::Overflow) {
                    MicroNoticeLevel::Info
                } else {
                    MicroNoticeLevel::Error
                };
                self.notice(level, detail.clone());
            }
            CoreViewEvent::CompactionFailed { detail, .. } => self.notice(
                MicroNoticeLevel::Error,
                format!("Compaction failed: {detail}"),
            ),
            CoreViewEvent::CompactionFinished { .. }
                if previous.is_some_and(|c| matches!(c.reason, CompactionReason::Threshold)) =>
            {
                self.notice(
                    MicroNoticeLevel::Info,
                    "Automatic compaction completed.".to_owned(),
                )
            }
            _ => {}
        }
    }

    fn apply(&self, envelope: &Envelope) -> bool {
        if self.closed.get() {
            return true;
        }
        let previous = self.state.borrow().conversation.compaction.clone();
        let applied = apply_envelope(&self.state.borrow().conversation, envelope);
        match applied {
            Ok(conversation) => {
                self.state.borrow_mut().conversation = conversation;
                for event in &envelope.events {
                    self.fold_event(event, previous.as_ref());
                }
                let usage = usage_view(
                    &self.cumulative.borrow(),
                    &self.state.borrow().conversation,
                    &self.model_runtime,
                );
                self.update(|state| state.usage = usage);
                true
            }
            Err(error) => {
                self.fail(error.to_string(), false);
                self.spawn_restart_watch();
                false
            }
        }
    }

    fn attach_watch(&self) {
        let Some(watch) = self.watch.borrow().clone() else {
            return;
        };
        let weak = self.myself.clone();
        watch.start(Rc::new(
            move |envelope: &Envelope| -> maho_agent::harness::pico3::types::Result<()> {
                if let Some(runtime) = weak.upgrade() {
                    runtime.apply(envelope);
                }
                Ok(())
            },
        ));
    }

    fn spawn_restart_watch(&self) {
        let Some(runtime) = self.myself.upgrade() else {
            return;
        };
        tokio::task::spawn_local(async move { runtime.restart_watch().await });
    }

    async fn restart_watch(&self) {
        if self.closed.get() || self.restarting_watch.replace(true) {
            return;
        }
        if let Some(watch) = self.watch.borrow_mut().take() {
            watch.stop();
        }
        let ctx = &*BACKGROUND_CONTEXT;
        let result = async {
            let watch = self.root.watch(ctx).await.map_err(|e| e.to_string())?;
            let cumulative = read_usage_accumulator(&self.harness, self.root.id).await?;
            Ok::<_, String>((watch, cumulative))
        }
        .await;
        match result {
            Ok((watch, cumulative)) => {
                let conversation = watch.view().clone();
                let usage = usage_view(&cumulative, &conversation, &self.model_runtime);
                *self.cumulative.borrow_mut() = cumulative;
                {
                    let mut state = self.state.borrow_mut();
                    state.conversation = conversation;
                    state.usage = usage;
                }
                *self.watch.borrow_mut() = Some(watch);
                self.attach_watch();
                self.publish();
            }
            Err(error) => self.fail(error, false),
        }
        self.restarting_watch.set(false);
    }

    async fn command<F>(&self, operation: F)
    where
        F: std::future::Future<Output = Result<(), String>>,
    {
        if self.state.borrow().fatal.is_some() || self.closed.get() {
            return;
        }
        if let Err(error) = operation.await {
            self.fail(error, false);
        }
    }

    fn clear_pending_auth(&self, error: &str) {
        if let Some(pending) = self.pending_auth.borrow_mut().take() {
            let _ = pending.sender.send(Err(error.to_owned()));
        }
    }

    fn abort_all(&self, message: &str) {
        for controller in self.refresh_controllers.borrow().iter() {
            controller.abort(Some(AbortReason::new("Error", message)));
        }
        if let Some(controller) = self.login_controller.borrow().as_ref() {
            controller.abort(Some(AbortReason::new("Error", message)));
        }
        self.clear_pending_auth(message);
    }

    /// senpi `stopOnLostLock`: mark the view fatal, abort refreshes and logins, then suspend the
    /// harness, which signals and joins every run and writes nothing (the gated storage refuses).
    async fn stop_on_lost_lock(&self) {
        self.update(|state| state.fatal = Some(LOST_LOCK_MESSAGE.to_owned()));
        self.notice(MicroNoticeLevel::Error, LOST_LOCK_MESSAGE.to_owned());
        self.abort_all(LOST_LOCK_MESSAGE);
        if let Err(error) = self.harness.suspend(&BACKGROUND_CONTEXT).await {
            log_lost_lock_stop_failure(&self.debug_log, &error);
        }
    }

    /// senpi `close()`.
    pub async fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        self.abort_all("Micro closed");
        if let Some(watch) = self.watch.borrow_mut().take() {
            watch.stop();
        }
        let _ = self.harness.close(&BACKGROUND_CONTEXT).await;
        FileSystem::cleanup(self.env.as_ref(), &BACKGROUND_CONTEXT).await;
        self.location.borrow_mut().release();
    }

    fn current_model(&self) -> Result<Model, String> {
        let reference = model_ref(self.state.borrow().conversation.config.get("model"))
            .ok_or("No model selected")?;
        self.model_runtime
            .get_model(&reference.provider, &reference.model_id)
            .ok_or_else(|| "Current model is unavailable".to_owned())
    }

    fn current_thinking(&self) -> ModelThinkingLevel {
        self.state
            .borrow()
            .conversation
            .config
            .get("thinkingLevel")
            .and_then(|value| serde_json::to_value(value).ok())
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or(ModelThinkingLevel::Off)
    }

    async fn send(&self, text: String, when_busy: WhenBusy) -> Result<(), String> {
        self.root
            .send(
                SendInput {
                    content: maho_ai::types::UserContent::Text(text),
                    request_id: None,
                    when_busy: Some(when_busy),
                },
                &BACKGROUND_CONTEXT,
            )
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    async fn refresh_models_inner(&self) -> Result<(), String> {
        let models = read_models_view(&self.model_runtime, true);
        self.update(|state| state.models = models);
        let controller = AbortController::new();
        self.refresh_controllers
            .borrow_mut()
            .push(controller.clone());
        let signal = controller.signal();
        let timer = {
            let controller = controller.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(MODEL_REFRESH_TIMEOUT_MS))
                    .await;
                controller.abort(None);
            })
        };
        let mut runtime = self.model_runtime.clone();
        let outcome = maho_interactive::model_catalog_refresh::refresh_model_catalogs(
            Rc::as_ptr(&self.harness) as usize as u64,
            &signal,
            move |signal| async move {
                runtime
                    .refresh(ModelsRefreshOptions {
                        signal: Some(signal),
                        ..Default::default()
                    })
                    .await
            },
        )
        .await;
        timer.abort();
        self.refresh_controllers
            .borrow_mut()
            .retain(|candidate| !candidate.signal().same_signal(&signal));
        let models = read_models_view(&self.model_runtime, false);
        self.update(|state| state.models = models);
        match outcome {
            Ok(result) if !result.errors.is_empty() => {
                self.notice(
                    MicroNoticeLevel::Warning,
                    format!(
                        "Could not refresh: {}",
                        result.errors.keys().cloned().collect::<Vec<_>>().join(", ")
                    ),
                );
                Ok(())
            }
            Ok(_) => Ok(()),
            Err(reason) => Err(reason.to_string()),
        }
    }

    async fn login_inner(
        &self,
        provider_id: String,
        auth_type: MicroAuthType,
    ) -> Result<(), String> {
        if self.login_controller.borrow().is_some() {
            return Err("A login is already running".to_owned());
        }
        let account = self
            .state
            .borrow()
            .models
            .accounts
            .iter()
            .find(|candidate| candidate.id == provider_id && candidate.auth_type == auth_type)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "Unknown login method: {provider_id}/{}",
                    match auth_type {
                        MicroAuthType::Oauth => "oauth",
                        MicroAuthType::ApiKey => "api_key",
                    }
                )
            })?;
        if !account.interactive {
            return Err(format!(
                "{} is configured outside pi",
                account.method_name.as_deref().unwrap_or("Authentication")
            ));
        }
        let controller = AbortController::new();
        *self.login_controller.borrow_mut() = Some(controller.clone());
        let auth_view = MicroAuthView {
            provider_id: provider_id.clone(),
            provider_name: account.name.clone(),
            auth_type,
            notices: Vec::new(),
            prompt: None,
        };
        self.update(|state| state.auth = Some(auth_view));
        let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let interaction = Arc::new(MicroAuthInteraction {
            signal: controller.signal(),
            events,
        });
        let runtime = self.model_runtime.clone();
        let provider_auth = match auth_type {
            MicroAuthType::Oauth => AuthType::OAuth,
            MicroAuthType::ApiKey => AuthType::ApiKey,
        };
        let login = runtime.login(&provider_id, provider_auth, interaction);
        let mut login = std::pin::pin!(login);
        let result = loop {
            tokio::select! {
                result = &mut login => break result,
                Some(event) = receiver.recv() => self.auth_event(event),
            }
        };
        self.clear_pending_auth("Login finished");
        *self.login_controller.borrow_mut() = None;
        let models = read_models_view(&self.model_runtime, false);
        self.update(|state| {
            state.auth = None;
            state.models = models;
        });
        result.map_err(|error| error.message)?;
        self.notice(
            MicroNoticeLevel::Info,
            format!("Logged in to {}.", account.name),
        );
        Ok(())
    }

    fn auth_event(&self, event: AuthChannelEvent) {
        match event {
            AuthChannelEvent::Notice(notice) => self.update(|state| {
                if let Some(auth) = state.auth.as_mut() {
                    auth.notices.push(notice);
                }
            }),
            AuthChannelEvent::Prompt { request, sender } => {
                self.clear_pending_auth("Login prompt replaced");
                let id = uuid::Uuid::new_v4().to_string();
                *self.pending_auth.borrow_mut() = Some(PendingAuth {
                    id: id.clone(),
                    sender,
                });
                self.update(|state| {
                    if let Some(auth) = state.auth.as_mut() {
                        auth.prompt = Some(MicroAuthPrompt { id, request });
                    }
                });
            }
        }
    }
}

enum AuthChannelEvent {
    Notice(AuthNotice),
    Prompt {
        request: AuthPromptRequest,
        sender: oneshot::Sender<Result<String, String>>,
    },
}

struct MicroAuthInteraction {
    signal: AbortSignal,
    events: tokio::sync::mpsc::UnboundedSender<AuthChannelEvent>,
}

#[async_trait::async_trait]
impl AuthInteraction for MicroAuthInteraction {
    fn signal(&self) -> Option<AbortSignal> {
        Some(self.signal.clone())
    }

    async fn prompt(&self, prompt: AuthPrompt) -> anyhow::Result<String> {
        let signal = prompt.signal.clone().unwrap_or_else(|| self.signal.clone());
        if signal.aborted() {
            anyhow::bail!("Login cancelled");
        }
        let (sender, receiver) = oneshot::channel();
        self.events
            .send(AuthChannelEvent::Prompt {
                request: AuthPromptRequest::from(prompt),
                sender,
            })
            .map_err(|_| anyhow::anyhow!("Login cancelled"))?;
        tokio::select! {
            answer = receiver => match answer {
                Ok(Ok(answer)) => Ok(answer),
                Ok(Err(error)) => Err(anyhow::anyhow!(error)),
                Err(_) => Err(anyhow::anyhow!("Login cancelled")),
            },
            () = signal.cancelled() => Err(anyhow::anyhow!("Login cancelled")),
        }
    }

    fn notify(&self, event: AuthEvent) {
        let _ = self
            .events
            .send(AuthChannelEvent::Notice(AuthNotice::from(event)));
    }
}

impl MicroViewSource for MicroRuntime {
    fn current(&self) -> MicroView {
        self.state.borrow().clone()
    }

    fn subscribe(&self, listener: Box<dyn Fn()>) -> Box<dyn FnOnce()> {
        let id = self.next_listener.replace(self.next_listener.get() + 1);
        self.listeners.borrow_mut().push((id, Rc::from(listener)));
        let weak = self.myself.clone();
        Box::new(move || {
            if let Some(runtime) = weak.upgrade() {
                runtime.listeners.borrow_mut().retain(|(key, _)| *key != id);
            }
        })
    }
}

impl MicroController for MicroRuntime {
    fn prompt(&self, text: String) -> LocalBoxFuture<'_, ()> {
        Box::pin(self.command(self.send(text, WhenBusy::Reject)))
    }
    fn steer(&self, text: String) -> LocalBoxFuture<'_, ()> {
        Box::pin(self.command(self.send(text, WhenBusy::Steer)))
    }
    fn follow_up(&self, text: String) -> LocalBoxFuture<'_, ()> {
        Box::pin(self.command(self.send(text, WhenBusy::FollowUp)))
    }
    fn compact(&self, instructions: Option<String>) -> LocalBoxFuture<'_, ()> {
        Box::pin(self.command(async move {
            self.root
                .collapse(instructions, &BACKGROUND_CONTEXT)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        }))
    }
    fn abort(&self) -> LocalBoxFuture<'_, ()> {
        Box::pin(self.command(async move {
            let compaction = self
                .state
                .borrow()
                .conversation
                .compaction
                .as_ref()
                .map(|c| c.task_id);
            self.root
                .abort(&BACKGROUND_CONTEXT)
                .await
                .map_err(|error| error.to_string())?;
            if let Some(task) = compaction {
                self.harness
                    .abort_task(task, &BACKGROUND_CONTEXT)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            Ok::<(), String>(())
        }))
    }
    fn cycle_thinking(&self) -> LocalBoxFuture<'_, ()> {
        Box::pin(self.command(async move {
            let model = self.current_model()?;
            if !model.reasoning {
                return Err("Current model does not support thinking".to_owned());
            }
            let levels = get_supported_thinking_levels(&model);
            let current = self.current_thinking();
            let next = levels
                .iter()
                .position(|level| *level == current)
                .map_or(0, |index| index + 1);
            let level = levels
                .get(next % levels.len().max(1))
                .copied()
                .unwrap_or(ModelThinkingLevel::Off);
            self.root
                .config_set(
                    object(serde_json::json!({"thinkingLevel": level})),
                    &BACKGROUND_CONTEXT,
                )
                .await
                .map_err(|error| error.to_string())
        }))
    }
    fn set_model(&self, reference: ModelRef) -> LocalBoxFuture<'_, ()> {
        Box::pin(self.command(async move {
            let model = self
                .model_runtime
                .get_model(&reference.provider, &reference.model_id)
                .ok_or_else(|| {
                    format!(
                        "Unknown model: {}/{}",
                        reference.provider, reference.model_id
                    )
                })?;
            let settings = (self.compaction)(&model).ok_or("Invalid compaction settings")?;
            let thinking = clamp_thinking_level(&model, self.current_thinking());
            let threshold = if settings.enabled {
                (model.context_window as i64 - settings.reserve_tokens).max(0)
            } else {
                0
            };
            self.root
                .config_set(
                    object(serde_json::json!({
                        "model": {"provider": reference.provider, "modelId": reference.model_id},
                        "thinkingLevel": thinking,
                        "threshold": threshold,
                        "keepRecent": settings.keep_recent_tokens,
                    })),
                    &BACKGROUND_CONTEXT,
                )
                .await
                .map_err(|error| error.to_string())
        }))
    }
    fn refresh_models(&self) -> LocalBoxFuture<'_, ()> {
        Box::pin(self.command(self.refresh_models_inner()))
    }
    fn login(&self, provider_id: String, auth_type: MicroAuthType) -> LocalBoxFuture<'_, ()> {
        Box::pin(self.command(self.login_inner(provider_id, auth_type)))
    }
    fn reply_auth(&self, request_id: String, answer: Option<String>) -> LocalBoxFuture<'_, ()> {
        Box::pin(self.command(async move {
            let matches = self
                .pending_auth
                .borrow()
                .as_ref()
                .is_some_and(|pending| pending.id == request_id);
            if !matches {
                return Ok::<(), String>(());
            }
            let pending = self.pending_auth.borrow_mut().take();
            self.update(|state| {
                if let Some(auth) = state.auth.as_mut() {
                    auth.prompt = None;
                }
            });
            if let Some(pending) = pending {
                let _ = pending
                    .sender
                    .send(answer.ok_or_else(|| "Login cancelled".to_owned()));
            }
            Ok(())
        }))
    }
    fn cancel_login(&self) -> LocalBoxFuture<'_, ()> {
        Box::pin(self.command(async move {
            if let Some(controller) = self.login_controller.borrow().as_ref() {
                controller.abort(Some(AbortReason::new("Error", "Login cancelled")));
            }
            self.clear_pending_auth("Login cancelled");
            self.update(|state| state.auth = None);
            Ok::<(), String>(())
        }))
    }
}

/// senpi `errorFields`: the error name, plus its code when it has one. A gated-storage refusal
/// carries the `FileLockLostError` code as its message prefix (see `lock_gated_storage`), so the
/// code is that prefix; nothing else of the message is kept.
fn lost_lock_error_fields(error: &Error) -> serde_json::Value {
    let name = match error {
        Error::Storage(_) => "StorageError",
        Error::Faulted(_) => "Faulted",
        Error::Closed => "Closed",
        _ => "Error",
    };
    let lock_lost = maho_core::file_lock::FileLockLostError::CODE;
    match error {
        Error::Storage(message) if message.starts_with(&format!("{lock_lost}:")) => {
            serde_json::json!({"name": name, "code": lock_lost})
        }
        _ => serde_json::json!({"name": name}),
    }
}

/// senpi `logLostLockStopFailure`. Names and codes only: error messages can carry absolute session paths.
fn log_lost_lock_stop_failure(debug_log: &DebugLog, error: &Error) {
    debug_log(
        "micro session stop after a lost lock failed",
        &serde_json::json!({
            "event": "micro_lost_lock_stop_failed",
            "error": lost_lock_error_fields(error),
        })
        .to_string(),
    );
}

/// senpi `UsageAccumulator`.
#[derive(Clone, Debug, Default)]
pub struct UsageAccumulator {
    seen: HashSet<u64>,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total_cost: f64,
    pub total_cost_unknown: bool,
    last_assistant_id: u64,
    pub last_cache_hit_rate: Option<f64>,
}

async fn read_usage_accumulator(
    harness: &Harness,
    conversation_id: u64,
) -> Result<UsageAccumulator, String> {
    let mut accumulator = UsageAccumulator::default();
    let mut before = None;
    loop {
        let page = harness
            .entries(
                EntryScan {
                    conversation_id,
                    kind: None,
                    with_head: None,
                    before,
                    limit: 256,
                },
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|error| error.to_string())?;
        for entry in &page {
            accumulate_usage(&mut accumulator, entry);
        }
        if page.len() < 256 {
            break;
        }
        before = page.last().map(|entry| entry.id);
    }
    Ok(accumulator)
}

/// senpi `accumulateUsage`.
pub fn accumulate_usage(accumulator: &mut UsageAccumulator, entry: &Entry) {
    if !accumulator.seen.insert(entry.id) {
        return;
    }
    let assistant = assistant_message(entry);
    let usage = assistant
        .as_ref()
        .map(|message| message.usage)
        .or_else(|| usage_record(entry));
    if let Some(usage) = usage {
        accumulator.input += usage.input;
        accumulator.output += usage.output;
        accumulator.cache_read += usage.cache_read;
        accumulator.cache_write += usage.cache_write;
        accumulator.total_cost += usage.cost.total;
        if usage.cost_unknown == Some(true) {
            accumulator.total_cost_unknown = true;
        }
    }
    if let Some(assistant) = assistant
        && !matches!(
            assistant.stop_reason,
            StopReason::Aborted | StopReason::Error
        )
        && entry.id > accumulator.last_assistant_id
    {
        accumulator.last_assistant_id = entry.id;
        let usage = assistant.usage;
        let prompt = usage.input + usage.cache_read + usage.cache_write;
        accumulator.last_cache_hit_rate =
            (prompt > 0).then(|| usage.cache_read as f64 / prompt as f64 * 100.0);
    }
}

/// senpi `usageView`.
pub fn usage_view(
    cumulative: &UsageAccumulator,
    conversation: &ConversationView,
    model_runtime: &ModelRuntime,
) -> MicroUsageView {
    let context_window = model_ref(conversation.config.get("model"))
        .and_then(|reference| model_runtime.get_model(&reference.provider, &reference.model_id))
        .map_or(0, |model| model.context_window);
    let newest_summary = conversation
        .entries
        .iter()
        .filter(|entry| entry.kind == "pi.summary")
        .map(|entry| entry.id)
        .next_back();
    let context_usage = conversation.entries.iter().rev().find_map(|entry| {
        let assistant = assistant_message(entry)?;
        (!matches!(
            assistant.stop_reason,
            StopReason::Aborted | StopReason::Error
        ) && newest_summary.is_none_or(|summary| entry.id > summary))
        .then_some(assistant.usage)
    });
    let context_tokens = context_usage.as_ref().and_then(context_token_count);
    let context_percent = match context_tokens {
        Some(tokens) if context_window > 0 => Some(tokens as f64 / context_window as f64 * 100.0),
        _ => None,
    };
    MicroUsageView {
        input: cumulative.input,
        output: cumulative.output,
        cache_read: cumulative.cache_read,
        cache_write: cumulative.cache_write,
        total_cost: cumulative.total_cost,
        total_cost_unknown: cumulative.total_cost_unknown,
        last_cache_hit_rate: cumulative.last_cache_hit_rate,
        context_tokens,
        context_window,
        context_percent,
    }
}

fn assistant_message(entry: &Entry) -> Option<AssistantMessage> {
    for stored in entry.model.iter().flatten() {
        if let Ok(Message::Assistant(message)) = stored.decode() {
            return Some(*message);
        }
    }
    if entry.kind != "pi.assistant" {
        return None;
    }
    let display = entry.data.as_ref()?.get("display")?;
    serde_json::from_value(serde_json::to_value(display).ok()?).ok()
}

fn usage_record(entry: &Entry) -> Option<Usage> {
    let usage = entry.data.as_ref()?.get("usage")?;
    serde_json::from_value(serde_json::to_value(usage).ok()?).ok()
}

fn context_token_count(usage: &Usage) -> Option<u64> {
    if let Some(context) = usage.context_usage {
        return context.map(|context| context.input + context.output);
    }
    Some(if usage.total_tokens > 0 {
        usage.total_tokens
    } else {
        usage.input + usage.output + usage.cache_read + usage.cache_write
    })
}

fn model_ref(value: Option<&JsonValue>) -> Option<ModelRef> {
    let value = serde_json::to_value(value?).ok()?;
    Some(ModelRef {
        provider: value.get("provider")?.as_str()?.to_owned(),
        model_id: value.get("modelId")?.as_str()?.to_owned(),
    })
}

#[cfg(test)]
mod lost_lock_log_tests {
    use super::*;

    #[test]
    fn a_gated_storage_refusal_logs_its_lock_lost_code_and_no_message() {
        let fields = lost_lock_error_fields(&Error::Storage("ELOCKLOST: /home/user/.pi/session.jsonl".to_owned()));
        assert_eq!(fields, serde_json::json!({"name": "StorageError", "code": "ELOCKLOST"}));
    }

    #[test]
    fn other_errors_log_their_name_only() {
        assert_eq!(lost_lock_error_fields(&Error::Storage("disk full".to_owned())), serde_json::json!({"name": "StorageError"}));
        assert_eq!(lost_lock_error_fields(&Error::Closed), serde_json::json!({"name": "Closed"}));
    }
}
