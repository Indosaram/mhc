use crate::claim_owner::{ClaimOwnerSeams, claim_onboarding_owned};
use crate::scan_runtime::{
    ONBOARDING_HISTORY_BUDGET_MS, OnboardingScanEnvironment, onboarding_history_summary,
};
use crate::session_history::{
    rpc::{SessionHistoryRpcOptions, register_session_history_rpc},
    tool::{CreateOnboardingSessionHistoryToolOptions, create_onboarding_session_history_tool},
};
use crate::state::{
    complete_onboarding, is_onboarding_complete, release_onboarding, reset_onboarding,
};
use crate::state_rpc::{OnboardingStateStore, StateWrite, register_onboarding_state_rpc};
use crate::tui_memory_runtime::{MemoryFactsApplier, TuiScan};
use crate::tui_memory_tools::{TuiMemoryToolsOptions, register_onboarding_memory_tools};
use crate::tui_scan_runtime::{TuiScanResult, run_tui_onboarding_scan};
use crate::understand_start::OnboardingUnderstandingSeams;
use maho_ext_api::*;
use maho_omo_start_work_continuation::agent_end_eligibility::read_agent_end_outcome;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

pub struct OnboardingComponent {
    pub state_dir: PathBuf,
    pub skills_root: String,
}

pub type ClaimOnboarding = Arc<dyn Fn(&Path, &ClaimOwnerSeams) -> bool + Send + Sync>;

/// Seams of the onboarding component; every field defaults to the real thing.
#[derive(Clone)]
pub struct OnboardingDependencies {
    /// The claim: defaults to the owner-recording claim (dead or pid-reused owner takeover, one winner).
    pub claim_onboarding: ClaimOnboarding,
    pub is_onboarding_claimable: ClaimOnboarding,
    /// Records a finished onboarding; a failure is swallowed (fail-open).
    pub complete_onboarding: StateWrite,
    /// Clears claim and completion for `omo.onboarding.state.reset`.
    pub reset_onboarding: StateWrite,
    /// Where the migration inventory's scan reads; defaults to what `omo setup` reads.
    pub scan_environment: Arc<dyn Fn() -> OnboardingScanEnvironment + Send + Sync>,
    /// The whole inventory source; replaces the scan's inventory when set.
    pub migration_facts: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
    /// The whole scan; `None` result onboards without one. Defaults to the shared onboarding scan.
    pub scan: Option<Arc<dyn Fn() -> Option<TuiScanResult> + Send + Sync>>,
    /// Fake-model seams for the understanding the TUI runs.
    pub understanding: Option<OnboardingUnderstandingSeams>,
    /// The session facts applier `onboarding_memory_apply` writes through; `None` when memory is off.
    pub applier: Option<Arc<dyn MemoryFactsApplier>>,
    /// Identity readers for claim owners; defaults to the real process table.
    pub claim_owner_seams: ClaimOwnerSeams,
    /// The bounded background summary of this machine's agent sessions; defaults to the scan runtime's.
    pub session_history: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
}

impl Default for OnboardingDependencies {
    fn default() -> Self {
        Self {
            claim_onboarding: Arc::new(claim_onboarding_owned),
            is_onboarding_claimable: Arc::new(|path, seams| {
                let state = crate::claim_owner::read_onboarding_state(path, seams);
                state.completed_at.is_none() && !state.in_progress
            }),
            complete_onboarding: Arc::new(complete_onboarding),
            reset_onboarding: Arc::new(reset_onboarding),
            scan_environment: Arc::new(OnboardingScanEnvironment::default),
            migration_facts: None,
            scan: None,
            understanding: None,
            applier: None,
            claim_owner_seams: ClaimOwnerSeams::default(),
            session_history: None,
        }
    }
}

/// An [`OnboardingComponent`] with explicit dependencies.
#[allow(clippy::module_name_repetitions)]
pub struct OnboardingComponentWith {
    pub base: OnboardingComponent,
    pub dependencies: OnboardingDependencies,
}

#[derive(Default)]
struct PendingRun {
    session_id: String,
    owned_marker: bool,
    active: bool,
    end: Option<JsonValue>,
}

type Pending = Arc<Mutex<Option<Option<PendingRun>>>>;

fn session_id(ctx: &ExtensionContext) -> String {
    ctx.session_manager.session_id().to_owned()
}

#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod test_support;

// Concurrent callers share one in-flight run (the lock is held while it runs) and a success is kept for the
// process; a failed run answers `None` and is dropped, so the next call starts a fresh one.
struct SyncOnce<T> {
    slot: Mutex<Option<T>>,
    run: Arc<dyn Fn() -> Option<T> + Send + Sync>,
}
impl<T: Clone> SyncOnce<T> {
    fn new(run: Arc<dyn Fn() -> Option<T> + Send + Sync>) -> Self {
        Self {
            slot: Mutex::new(None),
            run,
        }
    }
    fn get(&self) -> Option<T> {
        let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(kept) = slot.as_ref() {
            return Some(kept.clone());
        }
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.run)()))
            .ok()
            .flatten();
        *slot = run.clone();
        run
    }
}

impl OnboardingComponent {
    pub fn with_dependencies(
        self,
        dependencies: OnboardingDependencies,
    ) -> OnboardingComponentWith {
        OnboardingComponentWith {
            base: self,
            dependencies,
        }
    }
}

impl Extension for OnboardingComponentWith {
    fn register(&self, api: &mut ExtensionApi) {
        register_onboarding(api, &self.base, self.dependencies.clone());
    }
}

fn eligible(ctx: &ExtensionContext, session_kind: SessionKind, runtime: &ExtensionRuntime) -> bool {
    ctx.has_ui
        && session_kind == SessionKind::Interactive
        && runtime.get_flag("omo-senpi-onboarding-disabled") != Some(FlagValue::Boolean(true))
}

fn show_welcome(ctx: &ExtensionContext) {
    ctx.ui.set_widget(
        "omo-onboarding",
        Some(WidgetContent::Lines(vec![
            format!(
                "Welcome to {}. Ask anything.",
                maho_core::config::app_command()
            ),
        ])),
        ExtensionWidgetOptions::default(),
    );
}

impl Extension for OnboardingComponent {
    fn register(&self, api: &mut ExtensionApi) {
        register_onboarding(api, self, OnboardingDependencies::default());
    }
}

fn register_onboarding(
    api: &mut ExtensionApi,
    component: &OnboardingComponent,
    deps: OnboardingDependencies,
) {
    api.register_flag(
        "onboard",
        FlagType::Boolean {
            default: Some(false),
        },
        Some("Start the guided tour.".into()),
    );
    let runtime = api.runtime.clone();
    let session_kind = api.profile.session_kind;
    let state_dir = component.state_dir.clone();
    let skills_root = component.skills_root.clone();
    let process_forced = Arc::new(Mutex::new(false));
    // None is unclaimed; Some(None) is a successfully completed turn.
    let pending: Pending = Arc::new(Mutex::new(None::<Option<PendingRun>>));
    let claiming = Arc::new(AtomicBool::new(false));

    // The first onboarding turn awaits only `omo setup`'s sources (sessions: false): its rows, value summary
    // and migration inventory. Session digests are read by a second scan that starts only when
    // onboarding_understand runs, after consent. Each runs once per process, never at register(), and only
    // a success is kept: a failed run answers None and the next call retries it.
    let scan_run = |sessions: bool| -> Arc<dyn Fn() -> Option<Arc<TuiScanResult>> + Send + Sync> {
        match deps.scan.clone() {
            Some(scan) => Arc::new(move || scan().map(Arc::new)),
            None => {
                let environment = Arc::clone(&deps.scan_environment);
                Arc::new(move || Some(Arc::new(run_tui_onboarding_scan(&environment(), sessions))))
            }
        }
    };
    let inventory_scan = Arc::new(SyncOnce::new(scan_run(false)));
    let understanding_scan = Arc::new(SyncOnce::new(scan_run(true)));
    // An injected inventory replaces the scan's; the default scan then never starts for it.
    let injected = deps
        .migration_facts
        .clone()
        .map(|facts| Arc::new(SyncOnce::new(Arc::new(move || facts().map(Some)))));
    let injected_facts = deps.migration_facts.is_some();
    let scan_injected = deps.scan.is_some();

    // Session history is never awaited: it starts with the claim, runs bounded in the background and rides
    // the next turn when it lands; a hang or failure just leaves it out until the next claim.
    let history_started = Arc::new(AtomicBool::new(false));
    let start_history = {
        let (runtime, pending, started) = (
            runtime.clone(),
            Arc::clone(&pending),
            Arc::clone(&history_started),
        );
        let (summary, environment) = (
            deps.session_history.clone(),
            Arc::clone(&deps.scan_environment),
        );
        move || {
            if started.swap(true, Ordering::SeqCst) {
                return;
            }
            let Ok(actions) = runtime.message_actions() else {
                started.store(false, Ordering::SeqCst);
                return;
            };
            let (pending, started, summary, environment) = (
                Arc::clone(&pending),
                Arc::clone(&started),
                summary.clone(),
                Arc::clone(&environment),
            );
            std::thread::spawn(move || {
                let run =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &summary {
                        Some(summary) => summary(),
                        None => {
                            onboarding_history_summary(&environment(), ONBOARDING_HISTORY_BUDGET_MS)
                        }
                    }));
                let Ok(content) = run else {
                    started.store(false, Ordering::SeqCst);
                    return;
                };
                let claimed = pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .is_some();
                if let (Some(content), true) = (content, claimed) {
                    let message = CustomMessage {
                        custom_type: "omo-onboarding:history".into(),
                        content: vec![ToolContent::text(content)],
                        display: false,
                        details: None,
                    };
                    if let Err(error) = actions.send_message(
                        message,
                        SendMessageOptions {
                            trigger_turn: false,
                            deliver_as: Some(DeliverAs::NextTurn),
                        },
                    ) {
                        maho_ext_api::diagnostic(&format!(
                            "onboarding history delivery failed: {}",
                            error.message
                        ));
                    }
                }
            });
        }
    };
    type WithInventory = Arc<
        dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send>>
            + Send
            + Sync,
    >;
    let with_inventory: WithInventory = {
        let (inventory_scan, injected) = (Arc::clone(&inventory_scan), injected.clone());
        Arc::new(move |base: String| {
            let (inventory_scan, injected) = (Arc::clone(&inventory_scan), injected.clone());
            Box::pin(async move {
                let result = if injected_facts && !scan_injected {
                    None
                } else {
                    let scan = Arc::clone(&inventory_scan);
                    tokio::task::spawn_blocking(move || scan.get())
                        .await
                        .ok()
                        .flatten()
                };
                let facts = match injected {
                    None => result.as_ref().and_then(|r| r.inventory.clone()),
                    Some(injected) => tokio::task::spawn_blocking(move || injected.get().flatten())
                        .await
                        .ok()
                        .flatten(),
                };
                let mut content = base;
                if let Some(facts) = facts {
                    content.push_str(&format!(
                        " Migration inventory (from the same scan omo setup runs): {facts}"
                    ));
                }
                if let Some(result) = result {
                    content.push_str(&format!("\n\n{}", result.context));
                }
                content
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send>>
        })
    };

    let preview_inventory = Arc::clone(&with_inventory);
    let inventory_text = Arc::clone(&with_inventory);
    let with_inventory: WithInventory = Arc::new(move |base| {
        start_history();
        inventory_text(base)
    });

    let claim = {
        let (pending, claiming, state_dir, deps) = (
            Arc::clone(&pending),
            Arc::clone(&claiming),
            state_dir.clone(),
            deps.clone(),
        );
        move |ctx: &ExtensionContext, forced: bool| -> bool {
            if pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_some()
                || claiming.swap(true, Ordering::SeqCst)
            {
                return false;
            }
            let owned = (deps.claim_onboarding)(&state_dir, &deps.claim_owner_seams);
            claiming.store(false, Ordering::SeqCst);
            if !owned && !forced {
                return false;
            }
            *pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(Some(PendingRun {
                session_id: session_id(ctx),
                owned_marker: owned,
                active: false,
                end: None,
            }));
            true
        }
    };

    let scan_for_tools: TuiScan = {
        let scan = Arc::clone(&understanding_scan);
        Arc::new(move || scan.get())
    };
    if let Err(error) = register_onboarding_memory_tools(
        api,
        TuiMemoryToolsOptions {
            scan: scan_for_tools,
            seams: deps.understanding.clone(),
            applier: deps.applier.clone(),
        },
    ) {
        maho_ext_api::diagnostic(&format!(
            "onboarding memory tools registration failed: {}",
            error.message
        ));
    }
    api.register_tool(create_onboarding_session_history_tool(
        CreateOnboardingSessionHistoryToolOptions {
            state_dir: state_dir.to_string_lossy().into_owned(),
            scan_options: None,
            new_scan_id: None,
        },
    ));
    let reset_pending = Arc::clone(&pending);
    if let Err(error) = register_onboarding_state_rpc(
        api,
        OnboardingStateStore {
            state_dir: state_dir.clone(),
            complete_onboarding: Arc::clone(&deps.complete_onboarding),
            reset_onboarding: Arc::clone(&deps.reset_onboarding),
            on_reset: Arc::new(move || {
                *reset_pending.lock().unwrap_or_else(PoisonError::into_inner) = None;
            }),
            claim_owner_seams: deps.claim_owner_seams.clone(),
        },
    ) {
        maho_ext_api::diagnostic(&format!(
            "onboarding state rpc registration failed: {}",
            error.message
        ));
    }
    if let Err(error) = register_session_history_rpc(api, SessionHistoryRpcOptions::default()) {
        maho_ext_api::diagnostic(&format!(
            "onboarding session history rpc registration failed: {}",
            error.message
        ));
    }

    let startup_runtime = runtime.clone();
    let startup_dir = state_dir.clone();
    let startup_skills = skills_root.clone();
    let startup_forced = Arc::clone(&process_forced);
    let startup_pending = Arc::clone(&pending);
    let (startup_claim, startup_inventory) = (claim.clone(), Arc::clone(&with_inventory));
    api.on(
        EventKind::SessionStart,
        Arc::new(move |event, ctx| {
            let runtime = startup_runtime.clone();
            let state_dir = startup_dir.clone();
            let skills_root = startup_skills.clone();
            let process_forced = Arc::clone(&startup_forced);
            let pending = Arc::clone(&startup_pending);
            let (claim, with_inventory) = (startup_claim.clone(), Arc::clone(&startup_inventory));
            Box::pin(async move {
                let ExtensionEvent::SessionStart(event) = event else {
                    return Ok(EventResult::None);
                };
                if event.reason != SessionReason::Startup
                    || ctx.mode != ExtensionMode::Tui
                    || !eligible(ctx, session_kind, &runtime)
                {
                    return Ok(EventResult::None);
                }
                let forced = runtime.get_flag("onboard") == Some(FlagValue::Boolean(true))
                    && !*process_forced
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                if !forced {
                    if !is_onboarding_complete(&state_dir) {
                        show_welcome(ctx);
                    }
                    return Ok(EventResult::None);
                }
                if pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .is_some()
                {
                    return Ok(EventResult::None);
                }
                if !claim(ctx, true) {
                    return Ok(EventResult::None);
                }
                let owned_marker = pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_ref()
                    .and_then(Option::as_ref)
                    .is_some_and(|run| run.owned_marker);
                *process_forced
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = true;
                ctx.ui
                    .set_widget("omo-onboarding", None, ExtensionWidgetOptions::default());
                let content = with_inventory(format!(
                    "Follow {skills_root}/onboarding/SKILL.md. Greet first."
                ))
                .await;
                let message = CustomMessage {
                    custom_type: "omo-onboarding:bootstrap".into(),
                    content: vec![ToolContent::text(content)],
                    display: false,
                    details: None,
                };
                let actions = runtime.message_actions()?;
                if let Err(error) = actions.send_message(
                    message,
                    SendMessageOptions {
                        trigger_turn: true,
                        deliver_as: Some(DeliverAs::FollowUp),
                    },
                ) {
                    if owned_marker {
                        let _ = release_onboarding(&state_dir);
                    }
                    *pending.lock().unwrap_or_else(PoisonError::into_inner) = None;
                    if !is_onboarding_complete(&state_dir) {
                        show_welcome(ctx);
                    }
                    return Err(error);
                }
                Ok(EventResult::None)
            })
        }),
    );

    let prompt_runtime = runtime.clone();
    let prompt_skills = skills_root.clone();
    let prompt_forced = Arc::clone(&process_forced);
    let (prompt_claim, prompt_inventory) = (claim.clone(), Arc::clone(&with_inventory));
    let preview_pending = Arc::clone(&pending);
    let preview_claiming = Arc::clone(&claiming);
    let preview_dir = state_dir.clone();
    let preview_deps = deps.clone();
    api.on_with_options(EventKind::BeforeAgentStart,Arc::new(move |event,ctx| {
        let pending = Arc::clone(&preview_pending);
        let claiming = Arc::clone(&preview_claiming);
        let state_dir = preview_dir.clone();
        let deps = preview_deps.clone();
        let inventory_text = Arc::clone(&preview_inventory);
        let runtime=prompt_runtime.clone();let skills_root=prompt_skills.clone();
        let process_forced=Arc::clone(&prompt_forced);
        let (claim,with_inventory)=(prompt_claim.clone(),Arc::clone(&prompt_inventory));
        Box::pin(async move {
            let ExtensionEvent::BeforeAgentStart(event)=event else {return Ok(EventResult::None);};
            if event.trigger!="prompt" || !eligible(ctx,session_kind,&runtime) {return Ok(EventResult::None);}
            let forced=runtime.get_flag("onboard")==Some(FlagValue::Boolean(true))
                && !*process_forced.lock().unwrap_or_else(PoisonError::into_inner);
            if event.preview {
                if pending.lock().unwrap_or_else(PoisonError::into_inner).is_some() || claiming.load(Ordering::SeqCst) || (!forced && !(deps.is_onboarding_claimable)(&state_dir, &deps.claim_owner_seams)) { return Ok(EventResult::None); }
            } else if !claim(ctx,forced) {return Ok(EventResult::None);}
            if !event.preview && forced { *process_forced.lock().unwrap_or_else(PoisonError::into_inner)=true; }
            if !event.preview && ctx.mode==ExtensionMode::Tui {ctx.ui.set_widget("omo-onboarding",None,ExtensionWidgetOptions::default());}
            if ctx.session_manager.get_branch().iter().any(|entry| entry.kind=="custom_message" && entry.data.get("customType").and_then(JsonValue::as_str)==Some("omo-onboarding:context")) {
                return Ok(EventResult::None);
            }
            let base=if forced {format!("Follow {skills_root}/onboarding/SKILL.md. Tour using their message.")}
                else {format!("Answer fully; greet in their language; offer a one-line tour. On request/acceptance: Follow {skills_root}/onboarding/SKILL.md.")};
            let content=if event.preview {inventory_text(base).await} else {with_inventory(base).await};
            Ok(EventResult::BeforeAgentStart(BeforeAgentStartEventResult { system_prompt_parts: None,message:Some(CustomMessage {custom_type:"omo-onboarding:context".into(),content:vec![ToolContent::text(content)],display:false,details:None}),system_prompt:None}))
        })
    }), ExtensionHandlerOptions { preview_safe: true, ..Default::default() });

    for kind in [
        EventKind::AgentStart,
        EventKind::AgentEnd,
        EventKind::AgentSettled,
        EventKind::InputDisposition,
        EventKind::SessionAbort,
        EventKind::SessionShutdown,
    ] {
        let runtime = runtime.clone();
        let state_dir = state_dir.clone();
        let pending = Arc::clone(&pending);
        let session_kind = api.profile.session_kind;
        let complete = Arc::clone(&deps.complete_onboarding);
        api.on(kind,Arc::new(move |event,ctx| {
            let runtime=runtime.clone();let state_dir=state_dir.clone();let pending=Arc::clone(&pending);let session_kind=session_kind;
            let complete=Arc::clone(&complete);
            Box::pin(async move {
                let mut current=pending.lock().unwrap_or_else(PoisonError::into_inner);
                let Some(run)=current.as_mut().and_then(Option::as_mut) else {return Ok(EventResult::None);};
                if run.session_id!=session_id(ctx) {return Ok(EventResult::None);}
                let mut release=false;
                let mut settled_success=false;
                match (&kind,event) {
                    (EventKind::AgentStart,ExtensionEvent::AgentStart)=>{run.active=true;run.end=None;}
                    (EventKind::AgentEnd,ExtensionEvent::AgentEnd{messages,aborted,will_retry,..}) if run.active=>{
                        let payload=serde_json::json!({"messages":messages,"aborted":aborted.unwrap_or(false),"willRetry":will_retry.unwrap_or(false)});
                        let outcome=read_agent_end_outcome(&payload);
                        release=!outcome.will_retry && outcome.blocked_by.is_some();
                        run.end=Some(payload);
                    }
                    (EventKind::AgentSettled,ExtensionEvent::AgentSettled) if run.active=>{
                        release=read_agent_end_outcome(run.end.as_ref().unwrap_or(&JsonValue::Null)).blocked_by.is_some();
                        settled_success = !release;
                    }
                    (EventKind::InputDisposition,ExtensionEvent::InputDisposition{disposition,..}) if !run.active=>release=*disposition==InputDisposition::Rejected,
                    (EventKind::SessionAbort,ExtensionEvent::SessionAbort)|(EventKind::SessionShutdown,ExtensionEvent::SessionShutdown(_))=>release=true,
                    _=>{}
                }
                let owned=run.owned_marker;
                if release {
                    *current=None;
                } else if settled_success {
                    *current=Some(None);
                }
                drop(current);
                if settled_success {
                    // Fail-open like the claim: the held claim still keeps this install from re-onboarding.
                    if let Err(error)=complete(&state_dir) {maho_ext_api::diagnostic(&format!("onboarding completion not recorded: {error}"));}
                }
                if release {
                    if owned {release_onboarding(&state_dir).map_err(|error|ExtensionFailure::new(error.to_string()))?;}
                    if eligible(ctx,session_kind,&runtime) && ctx.mode==ExtensionMode::Tui && !is_onboarding_complete(&state_dir) {show_welcome(ctx);}
                }
                Ok(EventResult::None)
            })
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hermetic() -> OnboardingDependencies {
        OnboardingDependencies {
            session_history: Some(Arc::new(|| None)),
            ..Default::default()
        }
    }

    fn register(path: &std::path::Path) -> ExtensionApi {
        let mut api = ExtensionApi::new(
            LoadedExtension::new("onboarding", "/tmp".into(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            ExtensionRuntime::default(),
        );
        OnboardingComponent {
            state_dir: path.into(),
            skills_root: "/skills".into(),
        }
        .with_dependencies(hermetic())
        .register(&mut api);
        api
    }

    fn prompt(trigger: &str, preview: bool) -> ExtensionEvent {
        ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent {
            prompt: "hello".into(),
            images: None,
            system_prompt: "base".into(),
            system_prompt_options: Default::default(),
            trigger: trigger.into(),
            preview,
        })
    }

    #[test]
    fn ownership_uses_session_identity_without_a_leaf() {
        let ctx = test_support::context();
        assert_eq!(ctx.session_manager.get_leaf_id(), None);
        assert_eq!(session_id(&ctx), "session");
    }

    #[tokio::test]
    async fn pending_claim_is_released_by_abort_shutdown_and_rejection() {
        for mut event in [
            ExtensionEvent::SessionAbort,
            ExtensionEvent::SessionShutdown(SessionShutdownEvent {
                reason: SessionReason::Quit,
                target_session_file: None,
                signal: None,
            }),
            ExtensionEvent::InputDisposition {
                input_id: "input".into(),
                disposition: InputDisposition::Rejected,
            },
        ] {
            let root = tempfile::tempdir().expect("isolated onboarding directory");
            let api = register(root.path());
            let ctx = test_support::context();
            dispatch(&api, &mut prompt("prompt", false), &ctx).await;
            assert!(is_onboarding_complete(root.path()));
            dispatch(&api, &mut event, &ctx).await;
            assert!(!is_onboarding_complete(root.path()));
            assert!(matches!(
                dispatch(&api, &mut prompt("prompt", false), &ctx).await,
                EventResult::BeforeAgentStart(_)
            ));
        }
    }

    async fn dispatch(
        api: &ExtensionApi,
        event: &mut ExtensionEvent,
        ctx: &ExtensionContext,
    ) -> EventResult {
        api.registered.handlers[&event.kind()][0](event, ctx)
            .await
            .expect("onboarding event dispatch succeeds")
    }

    fn end(payload: JsonValue) -> ExtensionEvent {
        let messages=payload["messages"].as_array().expect("message array fixture").iter().map(|message| {
            let mut complete=serde_json::json!({"role":"assistant","content":[],"api":"test","provider":"test","model":"test","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"timestamp":0,"toolCallId":"call-1","toolName":"bash"});
            complete.as_object_mut().expect("message object").extend(message.as_object().expect("fixture object").clone());
            serde_json::from_value(complete).expect("valid agent message fixture")
        }).collect();
        ExtensionEvent::AgentEnd {
            messages,
            aborted: payload["aborted"].as_bool(),
            will_retry: payload["willRetry"].as_bool(),
            abort_source: None,
        }
    }

    #[tokio::test]
    async fn previews_and_non_prompt_turns_do_not_claim() {
        for (trigger, preview) in [("extension", false), ("delivery", false)] {
            let root = tempfile::tempdir().expect("isolated onboarding directory");
            let api = register(root.path());
            let ctx = test_support::context();
            assert!(matches!(
                dispatch(&api, &mut prompt(trigger, preview), &ctx).await,
                EventResult::None
            ));
            assert!(!is_onboarding_complete(root.path()));
            assert!(matches!(
                dispatch(&api, &mut prompt("prompt", false), &ctx).await,
                EventResult::BeforeAgentStart(_)
            ));
        }
    }

    #[tokio::test]
    async fn preview_matches_real_addition_without_claim_or_history() {
        let root = tempfile::tempdir().expect("isolated directory");
        let history = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = history.clone();
        let mut api = register(root.path());
        // Use a separate registration with a deterministic inventory and observable history seam.
        api.registered.handlers.clear();
        OnboardingComponent {
            state_dir: root.path().into(),
            skills_root: "/skills".into(),
        }
        .with_dependencies(OnboardingDependencies {
            migration_facts: Some(Arc::new(|| Some("inventory".into()))),
            session_history: Some(Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                None
            })),
            ..Default::default()
        })
        .register(&mut api);
        let ctx = test_support::context();
        let preview = dispatch(&api, &mut prompt("prompt", true), &ctx).await;
        assert!(matches!(preview, EventResult::BeforeAgentStart(_)));
        assert!(!is_onboarding_complete(root.path()));
        assert_eq!(history.load(Ordering::SeqCst), 0);
        let real = dispatch(&api, &mut prompt("prompt", false), &ctx).await;
        let content = |result: EventResult| match result {
            EventResult::BeforeAgentStart(result) => result.message.expect("addition").content,
            _ => panic!("missing addition"),
        };
        assert_eq!(
            serde_json::to_value(content(preview)).unwrap(),
            serde_json::to_value(content(real)).unwrap()
        );
        assert!(matches!(
            dispatch(&api, &mut prompt("prompt", true), &ctx).await,
            EventResult::None
        ));
    }

    #[tokio::test]
    async fn dead_owner_claim_is_previewable_without_takeover() {
        let root = tempfile::tempdir().unwrap();
        let seams = ClaimOwnerSeams {
            pid: Some(12345),
            get_pid_liveness: Some(Arc::new(|_| memory_core::locks::ProcessLiveness::Dead)),
            ..Default::default()
        };
        assert!(claim_onboarding_owned(root.path(), &seams));
        let marker = root.path().join(crate::state::ONBOARDING_MARKER);
        let before = std::fs::read(&marker).unwrap();
        let mut api = register(root.path());
        api.registered.handlers.clear();
        OnboardingComponent {
            state_dir: root.path().into(),
            skills_root: "/skills".into(),
        }
        .with_dependencies(OnboardingDependencies {
            claim_owner_seams: seams,
            migration_facts: Some(Arc::new(|| None)),
            ..hermetic()
        })
        .register(&mut api);
        assert!(matches!(
            dispatch(&api, &mut prompt("prompt", true), &test_support::context()).await,
            EventResult::BeforeAgentStart(_)
        ));
        assert_eq!(std::fs::read(marker).unwrap(), before);
    }

    #[tokio::test]
    async fn blocked_end_outcomes_release_and_retry_retains_until_settlement() {
        for payload in [
            serde_json::json!({"messages":[]}),
            serde_json::json!({"messages":[{"role":"assistant","stopReason":"error"}]}),
            serde_json::json!({"messages":[{"role":"assistant","stopReason":"aborted"}]}),
            serde_json::json!({"messages":[{"role":"assistant","stopReason":"toolUse","content":[],"stopDetails":{"type":"refusal"}}]}),
            serde_json::json!({"messages":[{"role":"assistant","stopReason":"stop"},{"role":"toolResult","isError":true,"content":[{"type":"text","text":"Command aborted"}]}]}),
            serde_json::json!({"messages":[],"aborted":true}),
            serde_json::json!({"messages":[],"willRetry":true}),
        ] {
            let root = tempfile::tempdir().expect("isolated onboarding directory");
            let api = register(root.path());
            let ctx = test_support::context();
            dispatch(&api, &mut prompt("prompt", false), &ctx).await;
            dispatch(&api, &mut ExtensionEvent::AgentStart, &ctx).await;
            let outcome = read_agent_end_outcome(&payload);
            assert!(outcome.blocked_by.is_some());
            dispatch(&api, &mut end(payload), &ctx).await;
            assert_eq!(is_onboarding_complete(root.path()), outcome.will_retry);
            dispatch(&api, &mut ExtensionEvent::AgentSettled, &ctx).await;
            assert!(!is_onboarding_complete(root.path()));
        }
    }

    #[tokio::test]
    async fn completion_blocks_reclaim_even_if_marker_is_removed() {
        let root = tempfile::tempdir().expect("isolated onboarding directory");
        let api = register(root.path());
        let ctx = test_support::context();
        dispatch(&api, &mut prompt("prompt", false), &ctx).await;
        dispatch(&api, &mut ExtensionEvent::AgentStart, &ctx).await;
        dispatch(
            &api,
            &mut end(serde_json::json!({"messages":[{"role":"assistant","stopReason":"stop"}]})),
            &ctx,
        )
        .await;
        dispatch(&api, &mut ExtensionEvent::AgentSettled, &ctx).await;
        release_onboarding(root.path()).expect("remove claim marker");
        assert!(matches!(
            dispatch(&api, &mut prompt("prompt", false), &ctx).await,
            EventResult::None
        ));
        assert!(crate::state::is_onboarding_completion_recorded(root.path()));
        assert!(!root.path().join(crate::state::ONBOARDING_MARKER).exists());
    }
}
