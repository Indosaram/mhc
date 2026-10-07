//! Assembly-owned concrete `KibitzerChildSpawner`: the CLI's in-process Kibitzer child.
//!
//! Behaviour of record: senpi `packages/omo-senpi/src/components/memory/kibitzer/sidecar.ts` - ONE
//! resident in-process child per bound main session, created lazily on the first wake - and the tool
//! registry `kibitzer/tools/index.ts` (`KIBITZER_SIDECAR_TOOL_NAMES`): exactly five member-scoped,
//! read-only closures (`read`, `grep`, `session_entries`, `memory`, `nudge`), never a builtin
//! passthrough, because only closures can enforce the output caps, the per-wake budget and redaction.
//!
//! The child's observable surface is ONE channel (`subscribe_observations`): the distinct
//! `tool_execution_start`/`tool_execution_end` phases and every `message_end`, forwarded verbatim from
//! the native `AgentSessionEvent` stream with the real `tool_call_id`, tool name, `is_error`, the
//! result's `details.terminate` stop hint and the structured `rejected` refusal code. Accepted nudges
//! and settlement stay on their own channels.
//!
//! The trait lives in `maho_omo_memory::kibitzer_child`; the CLI owns the concrete child because only
//! the CLI can build a native in-process `AgentSession` - the same `HostRuntimeFactory::create`
//! primitive `super::task_session::factory` uses for task children. The five member-scoped closures
//! and the resident persona are supplied by the memory composition ([`KibitzerChildResources`]); the
//! CLI never invents a tool and never fabricates a persona, so an empty resource set is a child with
//! no tools, never a builtin passthrough.
//!
//! The adapter implements the CURRENT async `KibitzerChild`/`KibitzerChildSpawner` ABI (boxed `Send`
//! futures; `abort` is a synchronous cancellation signal). Provider work is scheduled on the retained
//! runtime handle - never `Handle::block_on`, which would deadlock a sole current-thread reactor.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use maho_omo_memory::kibitzer_child::KibitzerChildObservation;
use maho_omo_memory::kibitzer_contract::{
    JudgeSettle, KibitzerChild, KibitzerChildSpawnInput, KibitzerChildSpawner,
};
use maho_omo_memory::kibitzer_sidecar_model::{
    KibitzerSidecarModelResolution, KibitzerSidecarModelUnavailableCause, KibitzerSidecarStartCode,
    KibitzerSidecarStartError,
};
use memory_core::recall::RecallNudge;

pub use maho_omo_memory::kibitzer_prompt_blocks::KIBITZER_SIDECAR_TOOL_NAMES;

/// The five member-scoped closures and the resident persona, supplied by the memory composition.
#[derive(Clone, Default)]
pub struct KibitzerChildResources {
    /// The resident persona text bound as the child's system prompt.
    pub persona: String,
    /// The member-scoped closures, in registry order.
    pub tools: Vec<maho_ext_api::ToolDefinition>,
}

/// Builds a child's resources for ONE spawn. Called PER CHILD, so the tool closures bind THAT child's
/// explicit parent session identity, its session-entries resolver and its current-wake budget; a
/// mount-wide Vec would share one budget/nudge slot across concurrent sessions.
pub type KibitzerChildResourcesFactory =
    Arc<dyn Fn(&KibitzerChildSpawnInput) -> KibitzerChildResources + Send + Sync>;

type ChildRegistry = Arc<Mutex<BTreeMap<String, Arc<CliKibitzerChild>>>>;

/// The CLI's concrete `KibitzerChildSpawner`.
pub struct CliKibitzerChildSpawner {
    executor: tokio::runtime::Handle,
    factory: maho_core::sdk::HostRuntimeFactory,
    cwd: String,
    agent_dir: String,
    /// The FULL omo config the resident model resolver reads (`LiveMemoryConfig`).
    config: super::memory_runtime::LiveMemoryConfig,
    /// `memory.recall.category`; the resolver falls back to the pinned default when absent.
    category: Option<String>,
    /// The session's registry snapshot, resolved at child start (per-session, never a stale mount
    /// capture). `None` -> the resolver returns `RegistrySnapshotUnavailable`.
    registry_for: Arc<dyn Fn(&str) -> Option<Arc<dyn maho_ext_api::ModelRegistry>> + Send + Sync>,
    resources: KibitzerChildResourcesFactory,
    children: ChildRegistry,
    inflight: Mutex<BTreeSet<String>>,
}

impl CliKibitzerChildSpawner {
    pub fn new(
        executor: tokio::runtime::Handle,
        factory: maho_core::sdk::HostRuntimeFactory,
        cwd: String,
        agent_dir: String,
        config: super::memory_runtime::LiveMemoryConfig,
        category: Option<String>,
        registry_for: Arc<dyn Fn(&str) -> Option<Arc<dyn maho_ext_api::ModelRegistry>> + Send + Sync>,
        resources: KibitzerChildResourcesFactory,
    ) -> Arc<Self> {
        Arc::new(Self { executor, factory, cwd, agent_dir, config, category, registry_for, resources, children: Arc::new(Mutex::new(BTreeMap::new())), inflight: Mutex::new(BTreeSet::new()) })
    }

    /// Production construction from the CLI's own install ports: ONE catalog/auth runtime per
    /// spawner (the same `ModelRuntime::create_sync` construction `host_runtime::host_model_runtime`
    /// uses), so every resident child shares the host's models.json/auth.json.
    pub fn from_agent_dir(
        executor: tokio::runtime::Handle,
        cwd: String,
        agent_dir: String,
        config: super::memory_runtime::LiveMemoryConfig,
        category: Option<String>,
        registry_for: Arc<dyn Fn(&str) -> Option<Arc<dyn maho_ext_api::ModelRegistry>> + Send + Sync>,
        resources: KibitzerChildResourcesFactory,
    ) -> Arc<Self> {
        let model_runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
            models_path: Some(std::path::Path::new(&agent_dir).join("models.json")),
            auth_path: Some(std::path::Path::new(&agent_dir).join("auth.json")),
            ..Default::default()
        });
        let factory = maho_core::sdk::HostRuntimeFactory {
            model_registry: maho_core::model_registry::ModelRegistry::new(model_runtime.clone()),
            model_runtime,
            extension_factories: Vec::new(),
        };
        Self::new(executor, factory, cwd, agent_dir, config, category, registry_for, resources)
    }

    /// Publish nudges accepted this turn to the resident child's `subscribe_nudges` listeners.
    pub fn publish_nudges(&self, session_id: &str, nudges: &[RecallNudge]) {
        let child = self.children.lock().unwrap_or_else(PoisonError::into_inner).get(session_id).cloned();
        if let Some(child) = child {
            child.publish_nudges(nudges);
        }
    }

    /// The resident child for a session, if one was spawned.
    pub fn resident(&self, session_id: &str) -> Option<Arc<CliKibitzerChild>> {
        self.children.lock().unwrap_or_else(PoisonError::into_inner).get(session_id).cloned()
    }

    async fn build_child(&self, input: &KibitzerChildSpawnInput) -> Result<Arc<CliKibitzerChild>, KibitzerSidecarStartError> {
        // Per-child resources: the factory binds THIS session's identity/budget.
        let resources = (self.resources)(input);
        // Resolve the category model + chain from the SESSION's registry snapshot (captured on the hook
        // pipeline). A missing snapshot is a typed transient refusal, never a factory/model fallback.
        let registry = (self.registry_for)(&input.session_id)
            .map(maho_omo_memory::model_registry_resolver::NativeMemoryModelRegistry);
        let config = (self.config)().map_err(|message| KibitzerSidecarStartError::new(KibitzerSidecarStartCode::RuntimeUnavailable, message))?;
        let resolution = maho_omo_memory::kibitzer_sidecar_model::resolve_kibitzer_sidecar_model(
            &maho_omo_memory::kibitzer_sidecar_model::KibitzerSidecarModelInput {
                category: self.category.as_deref(),
                config: &config,
                registry: registry.as_ref().map(|registry| registry as &dyn senpi_task::host::SenpiModelRegistry),
            },
        ).map_err(|error| KibitzerSidecarStartError::new(KibitzerSidecarStartCode::RuntimeUnavailable, error.message))?;
        let (model, thinking_selection, settings_manager) = match resolution {
            KibitzerSidecarModelResolution::Resolved { category, model, thinking, chain, .. } => {
                let (provider, id) = model.split_once('/')
                    .map(|(provider, id)| (provider.to_owned(), id.to_owned()))
                    .unwrap_or_else(|| (model.clone(), String::new()));
                // The category's leading model MUST exist in the session registry: an absent native
                // reference is a refusal that NAMES the category (the classifier reads the structured
                // `category` field, never the message), never a silent fall-through to factory defaults.
                let native_model = registry.as_ref().and_then(|registry| registry.0.find(&provider, &id)).ok_or_else(|| {
                    KibitzerSidecarStartError::new(KibitzerSidecarStartCode::CategoryUnavailable, format!("the resolved model {model} is absent from the session registry")).with_category(category)
                })?;
                // The resolver's normalized thinking survives into the child through the SAME selection
                // shape the native child options use; an unparseable level is a refusal, not a drop.
                let thinking_selection = match thinking.as_deref() {
                    None => None,
                    Some(level) => Some(maho_ai::types::ThinkingSelection {
                        level: maho_ai::types::ModelThinkingLevel::parse(level).ok_or_else(|| KibitzerSidecarStartError::new(
                            KibitzerSidecarStartCode::RuntimeUnavailable, format!("invalid child thinking level: {level}"),
                        ))?,
                        source: maho_ai::types::ThinkingSelectionSource::Explicit,
                        legacy_variant_id: None,
                    }),
                };
                let settings = super::task_runners::native_child_settings(
                    &senpi_task::runners::in_process::runtime_fallback_settings::create_runtime_fallback_settings(
                        chain.selected_model.as_deref(), chain.fallback_models.as_deref(), chain.retry.as_ref()));
                (native_model, thinking_selection, Some(settings))
            }
            KibitzerSidecarModelResolution::Unavailable { category, cause, missing_providers } => {
                let code = match cause {
                    KibitzerSidecarModelUnavailableCause::RegistrySnapshotUnavailable => KibitzerSidecarStartCode::RegistrySnapshotUnavailable,
                    KibitzerSidecarModelUnavailableCause::CategoryUnavailable => KibitzerSidecarStartCode::CategoryUnavailable,
                    KibitzerSidecarModelUnavailableCause::BeyondCategory => KibitzerSidecarStartCode::BeyondCategory,
                };
                return Err(KibitzerSidecarStartError { code, category: Some(category.clone()), missing_providers, message: format!("kibitzer model unavailable for category {category}") });
            }
        };
        let options = maho_core::sdk::CreateAgentSessionOptions {
            cwd: Some(self.cwd.clone()),
            agent_dir: Some(self.agent_dir.clone()),
            model: Some(model),
            thinking_selection,
            tools: Some(input.tools.clone()),
            custom_tools: resources.tools.clone(),
            system_prompt: Some(resources.persona.clone()),
            settings_manager,
            minimal_resources: true,
            ..Default::default()
        };
        let created = self.factory.create(options).await.map_err(|message| KibitzerSidecarStartError::new(KibitzerSidecarStartCode::SessionCreateFailed, message))?;
        created.session.set_system_prompt_sources(Some(resources.persona.clone()), Vec::new());
        Ok(Arc::new(CliKibitzerChild::new(
            Arc::new(created.session),
            self.executor.clone(),
            Arc::downgrade(&self.children),
            input.session_id.clone(),
        )))
    }
}

impl KibitzerChildSpawner for CliKibitzerChildSpawner {
    /// The producer's typed start error: a refusal names the STAGE that failed. Every failure the CLI
    /// itself can observe is a child-session creation failure (`SessionCreateFailed`); the registry,
    /// persona and model stages are produced by the memory resolver and pass through unchanged. No
    /// message classifier - only the structured code.
    fn spawn<'a>(&'a self, input: KibitzerChildSpawnInput) -> Pin<Box<dyn Future<Output = Result<Arc<dyn KibitzerChild>, KibitzerSidecarStartError>> + Send + 'a>> {
        Box::pin(async move {
            if self.children.lock().unwrap_or_else(PoisonError::into_inner).contains_key(&input.session_id) {
                return Err(KibitzerSidecarStartError::new(
                    KibitzerSidecarStartCode::SessionCreateFailed,
                    format!("a kibitzer child already exists for session {}", input.session_id),
                ));
            }
            if !self.inflight.lock().unwrap_or_else(PoisonError::into_inner).insert(input.session_id.clone()) {
                return Err(KibitzerSidecarStartError::new(
                    KibitzerSidecarStartCode::SessionCreateFailed,
                    format!("a kibitzer child is already being created for session {}", input.session_id),
                ));
            }
            let built = self.build_child(&input).await;
            self.inflight.lock().unwrap_or_else(PoisonError::into_inner).remove(&input.session_id);
            let child = built?;
            self.children.lock().unwrap_or_else(PoisonError::into_inner).insert(input.session_id.clone(), child.clone());
            Ok(child as Arc<dyn KibitzerChild>)
        })
    }
}

type NudgeListener = Arc<dyn Fn(RecallNudge) + Send + Sync>;
type ObservationListener = Arc<dyn Fn(KibitzerChildObservation) + Send + Sync>;

/// One resident in-process child: the native `AgentSession` plus the observable surface the sidecar
/// needs (steer/follow-up/abort, child observations, accepted nudges, settlement).
pub struct CliKibitzerChild {
    session: Arc<maho_core::agent_session::AgentSession>,
    executor: tokio::runtime::Handle,
    registry: Weak<Mutex<BTreeMap<String, Arc<CliKibitzerChild>>>>,
    session_id: String,
    nudges: Arc<Mutex<Vec<(u64, NudgeListener)>>>,
    observations: Arc<Mutex<Vec<(u64, ObservationListener)>>>,
    next_listener_id: Arc<Mutex<u64>>,
    /// ONE settlement record: the armed receivers (each with its expected turn) and the ACTIVE turn's
    /// accumulated outcome. Intermediate terminals ACCUMULATE; only the `AgentSettled` final boundary
    /// publishes. No separate cached outcome.
    settlement: Arc<Mutex<Settlement>>,
    /// CLI turn counter, advanced when a NEW turn begins (idle turn or follow-up).
    turn: Arc<AtomicU64>,
    /// The single in-flight prompt slot: `0` = none, `BUSY_PROMPT_OWNER` while a start reserves it,
    /// otherwise the owning turn generation. Reserved and installed under the settlement lock, so a
    /// boundary cannot clear a just-installed owner.
    prompt_owner: Arc<AtomicU64>,
    subscription: Mutex<Option<maho_core::agent_session::AgentSessionSubscription>>,
}

impl CliKibitzerChild {
    fn new(
        session: Arc<maho_core::agent_session::AgentSession>,
        executor: tokio::runtime::Handle,
        registry: Weak<Mutex<BTreeMap<String, Arc<CliKibitzerChild>>>>,
        session_id: String,
    ) -> Self {
        let nudges: Arc<Mutex<Vec<(u64, NudgeListener)>>> = Arc::new(Mutex::new(Vec::new()));
        let observations: Arc<Mutex<Vec<(u64, ObservationListener)>>> = Arc::new(Mutex::new(Vec::new()));
        let settlement: Arc<Mutex<Settlement>> = Arc::new(Mutex::new(Settlement::default()));
        let turn: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
        let prompt_owner: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
        let observation_listeners = observations.clone();
        let settle_out = settlement.clone();
        let turn_out = turn.clone();
        let owner_out = prompt_owner.clone();
        let subscription = session.subscribe(Arc::new(move |event| {
            let observation = match event {
                maho_ext_api::AgentSessionEvent::Agent(maho_ext_api::AgentEvent::ToolExecutionStart { tool_call_id, tool_name, .. }) => Some(KibitzerChildObservation::ToolStart {
                    tool_call_id: tool_call_id.clone(),
                    name: tool_name.clone(),
                }),
                maho_ext_api::AgentSessionEvent::Agent(maho_ext_api::AgentEvent::ToolExecutionEnd { tool_call_id, tool_name, result, is_error }) => Some(KibitzerChildObservation::ToolEnd {
                    tool_call_id: tool_call_id.clone(),
                    name: tool_name.clone(),
                    is_error: *is_error || result_is_error(result),
                    terminate: result_terminate(result),
                    refusal: result_refusal(result),
                }),
                maho_ext_api::AgentSessionEvent::Agent(maho_ext_api::AgentEvent::MessageEnd { message }) => Some(KibitzerChildObservation::MessageEnd { message: message.clone() }),
                maho_ext_api::AgentSessionEvent::AgentSettled => {
                    // The FINAL boundary: `publish` captures the current logical turn and the finished
                    // generation, releases, then publishes - all under the settlement lock.
                    publish(&settle_out, owner_out.as_ref(), turn_out.as_ref());
                    None
                }
                maho_ext_api::AgentSessionEvent::AgentEnd { aborted, .. } => {
                    // Intermediate: ACCUMULATE, never resolve.
                    observe(&settle_out, JudgeSettle { completed: !*aborted, cancelled: *aborted, failure_message: None });
                    None
                }
                maho_ext_api::AgentSessionEvent::SessionAbort => {
                    observe(&settle_out, JudgeSettle { completed: false, cancelled: true, failure_message: None });
                    None
                }
                maho_ext_api::AgentSessionEvent::ContinuationError { error_message } => {
                    observe(&settle_out, JudgeSettle { completed: false, cancelled: false, failure_message: Some(error_message.clone()) });
                    None
                }
                _ => None,
            };
            if let Some(observation) = observation {
                // Fan out OUTSIDE the listener lock so a listener may unsubscribe or re-enter.
                let listeners: Vec<ObservationListener> = observation_listeners.lock().unwrap_or_else(PoisonError::into_inner).iter().map(|(_, listener)| listener.clone()).collect();
                for listener in listeners {
                    listener(observation.clone());
                }
            }
        }));
        Self {
            session,
            executor,
            registry,
            session_id,
            nudges,
            observations,
            next_listener_id: Arc::new(Mutex::new(1)),
            settlement,
            turn,
            prompt_owner,
            subscription: Mutex::new(Some(subscription)),
        }
    }

    fn publish_nudges(&self, nudges: &[RecallNudge]) {
        for nudge in nudges {
            // Fan out OUTSIDE the listener lock so a listener may unsubscribe or re-enter.
            let listeners: Vec<NudgeListener> = self.nudges.lock().unwrap_or_else(PoisonError::into_inner).iter().map(|(_, listener)| listener.clone()).collect();
            for listener in listeners {
                listener(nudge.clone());
            }
        }
    }

    fn next_id(&self) -> u64 {
        let mut next = self.next_listener_id.lock().unwrap_or_else(PoisonError::into_inner);
        let id = *next;
        *next += 1;
        id
    }

    /// Start an idle turn on the retained executor and return once the prompt is ADMITTED, never after
    /// the whole provider lifetime. `session.prompt` awaits the entire turn, so awaiting it here would
    /// hide the turn's events from the wake and queue a deadline abort behind the provider lifetime.
    /// The spawned future is owned by the executor (never detached/dropped); its `AgentSettled` final
    /// boundary resolves the pre-armed receivers, and a failure resolves them via `resolve_now`.
    async fn run_idle_turn(&self, text: &str) -> Result<(), String> {
        // Reserve the in-flight slot AND install the turn generation atomically under the settlement
        // lock, so a boundary `publish` cannot clear a just-installed owner. A rejected duplicate
        // leaves the turn identity unchanged. The lock is released before any await.
        let generation = match claim_prompt_owner(&self.settlement, &self.prompt_owner, &self.turn) {
            Some(generation) => generation,
            None => return Err("a kibitzer child prompt is already in flight".to_owned()),
        };
        // The admission signal: `prompt_admitted` fires when the disposition is known, before the
        // provider completes, so the caller returns as soon as the turn is scheduled.
        let admitted: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>> = Arc::new(Mutex::new(None));
        let (admitted_sender, admitted_receiver) = tokio::sync::oneshot::channel::<()>();
        *admitted.lock().unwrap_or_else(PoisonError::into_inner) = Some(admitted_sender);
        let options = maho_core::agent_session::PromptOptions {
            prompt_admitted: Some(Arc::new(move |_disposition| {
                if let Some(sender) = admitted.lock().unwrap_or_else(PoisonError::into_inner).take() {
                    let _ = sender.send(());
                }
            })),
            ..Default::default()
        };
        let session = Arc::clone(&self.session);
        let settlement = Arc::clone(&self.settlement);
        let owner = Arc::clone(&self.prompt_owner);
        let text = text.to_owned();
        self.executor.spawn(async move {
            let result = session.prompt(&text, options).await;
            if let Err(error) = result {
                // Resolve the failure ONLY while THIS generation still owns the slot, under the
                // settlement lock, so a stale failed task can neither resolve a newer turn's receivers
                // nor erase its accumulator. `resolve_now` releases the ownership it guarded.
                resolve_now(&settlement, owner.as_ref(), generation, JudgeSettle { completed: false, cancelled: false, failure_message: Some(error) });
            }
            // Safety net: a no-op when the boundary (success) or `resolve_now` (failure) already
            // released THIS generation.
            let _ = owner.compare_exchange(generation, 0, Ordering::SeqCst, Ordering::SeqCst);
        });
        // Return on admission, not completion; a prompt that ended before admission drops the sender
        // and is surfaced as an error (its settlement is resolved by the spawned task).
        admitted_receiver.await.map_err(|_| "kibitzer child prompt ended before admission".to_owned())
    }
}

/// The native result's stop hint. `wrap_tool_definition` hardcodes `AgentToolResult::terminate` to
/// `None`, so the CLI tool bridge surfaces it under `details`; absent or non-boolean is `false`.
fn result_terminate(result: &serde_json::Value) -> bool {
    result.get("details").and_then(|details| details.get("terminate")).and_then(serde_json::Value::as_bool).unwrap_or(false)
}

/// The ORIGINAL result's error flag, carried under `details` when a terminating rejection is
/// delivered as `Ok` (the native event's `is_error` is then false). The observation's `is_error`
/// must reflect either source, so a `details.is_error = true` termination is never misreported as a
/// clean call. Absent or non-boolean is `false`.
fn result_is_error(result: &serde_json::Value) -> bool {
    result.get("details").and_then(|details| details.get("is_error")).and_then(serde_json::Value::as_bool).unwrap_or(false)
}

/// The structured refusal code from the native result's content-text JSON body (`rejected`), e.g.
/// `tool_budget_exceeded` for an exhausted wake budget or `invalid_pattern` for a bad argument.
/// `is_error` alone cannot discriminate those, so this code is the machine-consumed signal; `None`
/// when no text block carries a `rejected` string (a call that was not refused).
fn result_refusal(result: &serde_json::Value) -> Option<String> {
    let blocks = result.get("content")?.as_array()?;
    for block in blocks {
        if block.get("type").and_then(serde_json::Value::as_str) != Some("text") {
            continue;
        }
        let Some(text) = block.get("text").and_then(serde_json::Value::as_str) else { continue };
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text) {
            if let Some(code) = parsed.get("rejected").and_then(serde_json::Value::as_str) {
                return Some(code.to_owned());
            }
        }
    }
    None
}

/// ONE settlement record. Intermediate terminals ACCUMULATE into `pending`; only `publish` (the
/// `AgentSettled` boundary) or an explicit `resolve_now`/cancellation resolves the armed receivers.
#[derive(Default)]
struct Settlement {
    /// Armed receivers with their EXPECTED turn - per-receiver identity, so no shared metadata is
    /// overwritten when a second arm arrives.
    armed: Vec<(u64, tokio::sync::oneshot::Sender<JudgeSettle>)>,
    /// The active turn's accumulated outcome (from AgentEnd/SessionAbort/ContinuationError).
    pending: Option<JudgeSettle>,
    /// Outstanding cancellation owners (abort/dispose); terminals are suppressed while > 0.
    cancelling: usize,
}

/// Failure outranks cancellation, which outranks completion: a later weaker event never overwrites an
/// earlier stronger one for the same turn.
fn outranks(current: &JudgeSettle, incoming: &JudgeSettle) -> bool {
    let rank = |outcome: &JudgeSettle| if outcome.failure_message.is_some() { 2 } else if outcome.cancelled { 1 } else { 0 };
    rank(incoming) >= rank(current)
}

/// Accumulate an INTERMEDIATE terminal for the active turn; never resolve here.
fn observe(state: &Mutex<Settlement>, outcome: JudgeSettle) {
    let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
    if state.cancelling > 0 { return; }
    match &state.pending {
        Some(current) if !outranks(current, &outcome) => {}
        _ => state.pending = Some(outcome),
    }
}

/// Publish the accumulated outcome at the `AgentSettled` final boundary, once, to every receiver whose
/// expected turn has arrived; clear the accumulator for the next turn.
/// The in-flight slot's reservation token; never a valid generation (which starts at 1).
const BUSY_PROMPT_OWNER: u64 = u64::MAX;

/// Reserve the single in-flight prompt slot AND install the new turn generation, atomically under the
/// settlement lock. Serializing with `publish`/`resolve_now` (which take the same lock) means a
/// boundary cannot clear a just-installed owner. Returns the generation, or `None` when a prompt is
/// already in flight.
fn claim_prompt_owner(state: &Mutex<Settlement>, owner: &AtomicU64, turn: &AtomicU64) -> Option<u64> {
    let _guard = state.lock().unwrap_or_else(PoisonError::into_inner);
    if owner.compare_exchange(0, BUSY_PROMPT_OWNER, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return None;
    }
    let generation = turn.fetch_add(1, Ordering::SeqCst) + 1;
    owner.store(generation, Ordering::SeqCst);
    Some(generation)
}

/// Advance the logical turn for a queued running follow-up, under the settlement lock so its ordering
/// with a boundary `publish` capture is serialized. Returns the new generation.
fn advance_turn_for_followup(state: &Mutex<Settlement>, turn: &AtomicU64) -> u64 {
    let _guard = state.lock().unwrap_or_else(PoisonError::into_inner);
    turn.fetch_add(1, Ordering::SeqCst) + 1
}

/// Publish the accumulated outcome at the `AgentSettled` final boundary. Captures the CURRENT logical
/// turn BEFORE releasing the owner (a running follow-up advances `turn` past the prompt's own
/// generation, so publishing the current turn resolves that queued follow-up's armed receiver), then
/// releases a real generation (never a `BUSY_PROMPT_OWNER` reservation). Serialized by the settlement
/// lock, so it cannot race a claim or a failure resolve.
fn publish(state: &Mutex<Settlement>, owner: &AtomicU64, turn: &AtomicU64) {
    let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
    if state.cancelling > 0 { return; }
    let current_turn = turn.load(Ordering::SeqCst);
    let finished = owner.load(Ordering::SeqCst);
    if finished != 0 && finished != BUSY_PROMPT_OWNER {
        let _ = owner.compare_exchange(finished, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
    let outcome = state.pending.take().unwrap_or(JudgeSettle { completed: true, cancelled: false, failure_message: None });
    let mut remaining = Vec::new();
    for (expected, sender) in state.armed.drain(..) {
        if expected <= current_turn { let _ = sender.send(outcome.clone()); } else { remaining.push((expected, sender)); }
    }
    state.armed = remaining;
}

/// Resolve the armed receivers for an explicit trigger failure, ONLY while `generation` still owns the
/// slot: a stale failed task cannot resolve a newer turn's receivers or erase its accumulator. Releases
/// the ownership it guarded, under the same settlement lock `publish` uses.
fn resolve_now(state: &Mutex<Settlement>, owner: &AtomicU64, generation: u64, outcome: JudgeSettle) {
    let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
    if owner.load(Ordering::SeqCst) != generation { return; }
    for (_, sender) in state.armed.drain(..) {
        let _ = sender.send(outcome.clone());
    }
    state.pending = None;
    owner.store(0, Ordering::SeqCst);
}

/// Arm a receiver for the CURRENT wake, recording the turn it expects. Installing the receiver is the
/// only step; a terminal that fires immediately after finds it (resolution takes the same lock).
fn arm_settle(state: &Mutex<Settlement>, turn: u64) -> tokio::sync::oneshot::Receiver<JudgeSettle> {
    let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
    let (sender, receiver) = tokio::sync::oneshot::channel();
    state.armed.push((turn, sender));
    receiver
}

/// Enter a cancellation window and take the armed receivers: the caller owns their outcome.
fn begin_cancellation(state: &Mutex<Settlement>) -> Vec<tokio::sync::oneshot::Sender<JudgeSettle>> {
    let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
    state.cancelling += 1;
    std::mem::take(&mut state.armed).into_iter().map(|(_, sender)| sender).collect()
}

/// Leave ONE cancellation window; the gate reopens only when the last owner leaves.
fn end_cancellation(state: &Mutex<Settlement>) {
    let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
    state.cancelling = state.cancelling.saturating_sub(1);
}

/// Resolve the captured receivers as cancelled (after the abort/dispose has finished).
fn resolve_cancelled(captured: Vec<tokio::sync::oneshot::Sender<JudgeSettle>>) {
    for sender in captured {
        let _ = sender.send(JudgeSettle { completed: false, cancelled: true, failure_message: None });
    }
}

impl KibitzerChild for CliKibitzerChild {
    fn steer<'a>(&'a self, text: &'a str) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            if self.session.is_idle() { self.run_idle_turn(text).await }
            else { self.session.steer(text, None, Default::default()).await }
        })
    }

    fn follow_up<'a>(&'a self, text: &'a str) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            if self.session.is_idle() {
                self.run_idle_turn(text).await
            } else {
                // Advance under the settlement lock so the ordering with a boundary `publish` capture
                // is serialized.
                advance_turn_for_followup(&self.settlement, &self.turn);
                self.session.follow_up(text, None, Default::default()).await
            }
        })
    }

    fn abort(&self) {
        let captured = begin_cancellation(&self.settlement);
        let settlement = Arc::clone(&self.settlement);
        let session = Arc::clone(&self.session);
        self.executor.spawn(async move {
            session.abort().await;
            resolve_cancelled(captured);
            end_cancellation(&settlement);
        });
    }

    fn subscribe_nudges(&self, listener: NudgeListener) -> Box<dyn FnOnce() + Send> {
        let id = self.next_id();
        self.nudges.lock().unwrap_or_else(PoisonError::into_inner).push((id, listener));
        let nudges = self.nudges.clone();
        Box::new(move || {
            nudges.lock().unwrap_or_else(PoisonError::into_inner).retain(|(listener_id, _)| *listener_id != id);
        })
    }

    fn subscribe_observations(&self, listener: ObservationListener) -> Box<dyn FnOnce() + Send> {
        let id = self.next_id();
        self.observations.lock().unwrap_or_else(PoisonError::into_inner).push((id, listener));
        let observations = self.observations.clone();
        Box::new(move || {
            observations.lock().unwrap_or_else(PoisonError::into_inner).retain(|(listener_id, _)| *listener_id != id);
        })
    }

    /// The settle future is `'static` (it owns its receiver), so a caller can spawn it independently
    /// of the child borrow.
    fn settle(&self) -> Pin<Box<dyn Future<Output = JudgeSettle> + Send + 'static>> {
        let receiver = arm_settle(&self.settlement, self.turn.load(Ordering::SeqCst));
        Box::pin(async move {
            receiver.await.unwrap_or(JudgeSettle {
                completed: false,
                cancelled: false,
                failure_message: Some("kibitzer child settled without a settle event".to_owned()),
            })
        })
    }

    fn dispose<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            let captured = begin_cancellation(&self.settlement);
            self.session.emit_session_shutdown(maho_ext_api::SessionReason::Quit).await;
            self.session.dispose().await;
            resolve_cancelled(captured);
            end_cancellation(&self.settlement);
            self.subscription.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(registry) = self.registry.upgrade() {
                registry.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.session_id);
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod settlement_tests {
    use super::{advance_turn_for_followup, arm_settle, begin_cancellation, claim_prompt_owner, end_cancellation, observe, publish, resolve_cancelled, resolve_now, Settlement};
    use maho_omo_memory::kibitzer_contract::JudgeSettle;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;
    use tokio::sync::oneshot::error::TryRecvError;

    fn settlement_state() -> Mutex<Settlement> {
        Mutex::new(Settlement::default())
    }
    fn completed() -> JudgeSettle {
        JudgeSettle { completed: true, cancelled: false, failure_message: None }
    }
    fn cancelled() -> JudgeSettle {
        JudgeSettle { completed: false, cancelled: true, failure_message: None }
    }
    fn failed(message: &str) -> JudgeSettle {
        JudgeSettle { completed: false, cancelled: false, failure_message: Some(message.to_owned()) }
    }

    /// T1 arm-before-event: a receiver armed at the current turn resolves when the final boundary
    /// publishes. No sleeps: `try_recv` reads the oneshot state directly.
    #[test]
    fn an_armed_receiver_resolves_at_the_final_boundary() {
        let state = settlement_state();
        let owner = AtomicU64::new(0);
        let turn = AtomicU64::new(0);
        let mut receiver = arm_settle(&state, 0);
        observe(&state, completed());
        publish(&state, &owner, &turn);
        assert_eq!(receiver.try_recv().expect("published"), completed());
    }

    /// T2 an INTERMEDIATE terminal accumulates but never resolves; only publish delivers it.
    #[test]
    fn an_intermediate_terminal_does_not_resolve_before_publish() {
        let state = settlement_state();
        let owner = AtomicU64::new(0);
        let turn = AtomicU64::new(0);
        let mut receiver = arm_settle(&state, 0);
        observe(&state, failed("provider error"));
        // Exact `Empty` (not `Closed`): a dropped sender must not mask a premature resolution.
        assert_eq!(receiver.try_recv().unwrap_err(), TryRecvError::Empty, "an intermediate AgentEnd must not resolve the wake");
        publish(&state, &owner, &turn);
        assert_eq!(receiver.try_recv().expect("published"), failed("provider error"));
    }

    /// T3 precedence: failure outranks cancel outranks completion, regardless of arrival order.
    #[test]
    fn a_stronger_outcome_is_never_overwritten_by_a_weaker_one() {
        let failure_first = settlement_state();
        let failure_owner = AtomicU64::new(0);
        let failure_turn = AtomicU64::new(0);
        let mut receiver = arm_settle(&failure_first, 0);
        observe(&failure_first, completed());
        observe(&failure_first, failed("boom"));
        observe(&failure_first, completed());
        publish(&failure_first, &failure_owner, &failure_turn);
        assert_eq!(receiver.try_recv().expect("published"), failed("boom"));

        let cancel_first = settlement_state();
        let cancel_owner = AtomicU64::new(0);
        let cancel_turn = AtomicU64::new(0);
        let mut receiver = arm_settle(&cancel_first, 0);
        observe(&cancel_first, cancelled());
        observe(&cancel_first, completed());
        publish(&cancel_first, &cancel_owner, &cancel_turn);
        assert_eq!(receiver.try_recv().expect("published"), cancelled());
    }

    /// T4 multiple receivers: one publish fans out to every receiver whose turn has arrived.
    #[test]
    fn one_publish_resolves_every_armed_receiver() {
        let state = settlement_state();
        let owner = AtomicU64::new(0);
        let turn = AtomicU64::new(0);
        let mut first = arm_settle(&state, 0);
        let mut second = arm_settle(&state, 0);
        observe(&state, completed());
        publish(&state, &owner, &turn);
        assert_eq!(first.try_recv().expect("first"), completed());
        assert_eq!(second.try_recv().expect("second"), completed());
    }

    /// T5a cancellation capture: `begin_cancellation` takes the armed receivers and suppresses further
    /// accumulation; `resolve_cancelled` delivers a cancel to the captured receiver.
    #[test]
    fn cancellation_captures_the_armed_receivers_and_suppresses_accumulation() {
        let state = settlement_state();
        let owner = AtomicU64::new(0);
        let turn = AtomicU64::new(0);
        let mut receiver = arm_settle(&state, 0);
        let captured = begin_cancellation(&state);
        observe(&state, completed());
        publish(&state, &owner, &turn);
        assert_eq!(receiver.try_recv().unwrap_err(), TryRecvError::Empty, "the gate suppresses terminals while cancelling");
        resolve_cancelled(captured);
        assert_eq!(receiver.try_recv().expect("cancelled"), cancelled());
        end_cancellation(&state);
        let mut next = arm_settle(&state, 0);
        observe(&state, completed());
        publish(&state, &owner, &turn);
        assert_eq!(next.try_recv().expect("gate reopened"), completed());
    }

    /// T5b explicit trigger error: `resolve_now` delivers the trigger failure to the armed receiver
    /// without waiting for a settled boundary.
    #[test]
    fn an_explicit_trigger_error_resolves_the_armed_receiver() {
        let state = settlement_state();
        let owner = AtomicU64::new(1);
        let mut receiver = arm_settle(&state, 1);
        resolve_now(&state, &owner, 1, failed("trigger failed"));
        assert_eq!(receiver.try_recv().expect("explicit error"), failed("trigger failed"));
        assert!(state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pending.is_none(), "the accumulator is cleared");
        assert_eq!(owner.load(Ordering::SeqCst), 0, "the guarded ownership is released");
    }

    /// T6 duplicate rejection / no turn shift: the REAL `claim_prompt_owner` reserves before advancing,
    /// so a refused duplicate leaves the turn identity (and the original receiver) unchanged.
    #[test]
    fn a_rejected_duplicate_start_does_not_shift_the_turn() {
        let state = settlement_state();
        let owner = AtomicU64::new(0);
        let turn = AtomicU64::new(5);
        assert_eq!(claim_prompt_owner(&state, &owner, &turn), Some(6), "the first start claims");
        assert_eq!(turn.load(Ordering::SeqCst), 6, "the claimed start advanced the turn");
        assert_eq!(claim_prompt_owner(&state, &owner, &turn), None, "the duplicate is refused");
        assert_eq!(turn.load(Ordering::SeqCst), 6, "a rejected duplicate must not shift the turn identity");
        assert_eq!(owner.load(Ordering::SeqCst), 6, "the owner is still the first generation");
    }

    /// T7 queued running follow-up terminal: a running follow-up advanced the logical turn under the
    /// lock, so the final boundary publishes the CURRENT turn and resolves the follow-up's receiver.
    #[test]
    fn publish_resolves_a_queued_follow_up_at_the_current_logical_turn() {
        let state = settlement_state();
        let owner = AtomicU64::new(1);
        let turn = AtomicU64::new(1);
        assert_eq!(advance_turn_for_followup(&state, &turn), 2, "the follow-up advances the logical turn");
        let mut receiver = arm_settle(&state, 2);
        observe(&state, completed());
        publish(&state, &owner, &turn);
        assert_eq!(receiver.try_recv().expect("published at the current logical turn"), completed());
        assert_eq!(owner.load(Ordering::SeqCst), 0, "the finished generation released the slot");
    }

    /// T8 stale failure accumulator preservation: a failure for a NON-owning generation neither
    /// resolves a newer turn's receiver nor clears its accumulator.
    #[test]
    fn a_stale_failure_does_not_touch_a_newer_turn() {
        let state = settlement_state();
        let owner = AtomicU64::new(2);
        observe(&state, completed());
        let mut receiver = arm_settle(&state, 2);
        resolve_now(&state, &owner, 1, failed("stale"));
        assert_eq!(receiver.try_recv().unwrap_err(), TryRecvError::Empty, "a stale failure must not resolve");
        assert!(state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pending.is_some(), "the newer accumulator is preserved");
        assert_eq!(owner.load(Ordering::SeqCst), 2, "a stale failure must not release the newer owner");
    }
}
