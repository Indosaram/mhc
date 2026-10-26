//! Unified runner outcome types and the in-process child handle (`runners/in-process/child-handle.ts`).

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;

use serde::Serialize;
use serde_json::Value;

use crate::host::HostError;
use crate::manager::child_handle::{ManagedChildHandle, ManagedChildListener, Unsubscribe};
use crate::runners::rpc::handle::managed_event;
use crate::runners::rpc::turn_outcome::extract_assistant_text;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RunnerFailureKind {
    #[serde(rename = "child-prompt-failed")]
    ChildPromptFailed,
    #[serde(rename = "child-turn-failed")]
    ChildTurnFailed,
    #[serde(rename = "session-create-failed")]
    SessionCreateFailed,
    #[serde(rename = "depth-exceeded")]
    DepthExceeded,
    #[serde(rename = "model_unavailable")]
    ModelUnavailable,
    #[serde(rename = "tools_unavailable")]
    ToolsUnavailable,
    #[serde(rename = "session_unavailable")]
    SessionUnavailable,
}

impl RunnerFailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChildPromptFailed => "child-prompt-failed",
            Self::ChildTurnFailed => "child-turn-failed",
            Self::SessionCreateFailed => "session-create-failed",
            Self::DepthExceeded => "depth-exceeded",
            Self::ModelUnavailable => "model_unavailable",
            Self::ToolsUnavailable => "tools_unavailable",
            Self::SessionUnavailable => "session_unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerFailure {
    pub kind: RunnerFailureKind,
    pub message: String,
}

impl RunnerFailure {
    pub fn new(kind: RunnerFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerOutcome {
    Completed {
        final_response: String,
    },
    Error {
        failure: RunnerFailure,
        killed: bool,
    },
    Cancelled,
}

impl RunnerOutcome {
    pub fn completed(final_response: impl Into<String>) -> Self {
        Self::Completed {
            final_response: final_response.into(),
        }
    }

    pub fn error(kind: RunnerFailureKind, message: impl Into<String>) -> Self {
        Self::Error {
            failure: RunnerFailure::new(kind, message),
            killed: false,
        }
    }
}

/// How a settled turn is judged (`ChildCompletionPolicy` in `runners/in-process/child-handle.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChildCompletionPolicy {
    /// `"final-text"`: a turn completes only with assistant text. The default.
    #[default]
    FinalText,
    /// `"turn"`: any normally settled turn completes, even with no assistant text - for tool-only
    /// children whose deliverable is a side effect.
    Turn,
}

/// Host session events are untrusted JSON (`{ type, message? }`).
pub type ChildSessionListener = Arc<dyn Fn(&Value) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkpoolChildIdentity {
    pub task_id: String,
    pub state_dir: String,
    pub run_epoch: u32,
}

/// The host agent-session surface an in-process child drives. `prompt` blocks until the turn ends.
pub trait ChildSession: Send + Sync {
    fn priority_pairing_registry(
        &self,
    ) -> Option<&dyn crate::runners::pinned_model_equivalence::PriorityPairingRegistry> {
        None
    }
    fn model(&self) -> Option<crate::runners::pinned_model_equivalence::ModelIdentity> {
        None
    }
    fn hold_session(&self) -> Option<Arc<dyn super::native_run_pause::RunHoldSession>> {
        None
    }
    fn run_pause(&self) -> Option<&dyn crate::pause::native_child_pause::NativeRunPause> {
        None
    }
    fn session_id(&self) -> String;
    fn prompt(&self, text: &str) -> Result<(), HostError>;
    fn steer(&self, text: &str) -> Result<(), HostError>;
    fn follow_up(&self, text: &str) -> Result<(), HostError>;
    fn rebind_workpool_identity(
        &self,
        _identity: WorkpoolChildIdentity,
    ) -> Option<Result<(), HostError>> {
        None
    }
    fn abort(&self) -> Result<(), HostError>;
    fn subscribe(&self, listener: ChildSessionListener) -> Unsubscribe;
    fn get_last_assistant_text(&self) -> Option<String>;
    fn park_idle_memory(&self) {}
    fn dispose(&self);
}

#[derive(Default)]
struct TurnObservation {
    text: Option<String>,
    stop_reason: Option<String>,
    error_message: Option<String>,
    baseline: Option<String>,
}

impl TurnObservation {
    fn observe(&mut self, event: &Value) {
        if event.get("type").and_then(Value::as_str) != Some("message_end") {
            return;
        }
        let Some(message) = event
            .get("message")
            .filter(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
        else {
            return;
        };
        if let Some(text) = extract_assistant_text(message) {
            self.text = Some(text);
        }
        let string = |key: &str| message.get(key).and_then(Value::as_str).map(str::to_string);
        self.stop_reason = string("stopReason");
        self.error_message = string("errorMessage");
    }
}

struct TrackedState {
    aborted: bool,
    disposed: bool,
    turn: u64,
    outcome: Option<RunnerOutcome>,
    outcomes: std::collections::HashMap<u64, RunnerOutcome>,
    observation: TurnObservation,
}

struct Tracked {
    state: Mutex<TrackedState>,
    settled: Condvar,
}

impl Tracked {
    fn lock(&self) -> MutexGuard<'_, TrackedState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn turn_outcome(
    session: &dyn ChildSession,
    observation: &TurnObservation,
    completion: ChildCompletionPolicy,
) -> RunnerOutcome {
    if let Some(reason @ ("error" | "aborted")) = observation.stop_reason.as_deref() {
        let message = observation
            .error_message
            .clone()
            .unwrap_or_else(|| format!("child turn ended with stopReason \"{reason}\""));
        return RunnerOutcome::error(RunnerFailureKind::ChildTurnFailed, message);
    }
    if let Some(text) = &observation.text {
        return RunnerOutcome::completed(text.clone());
    }
    match session.get_last_assistant_text() {
        Some(last) if !last.is_empty() && Some(&last) != observation.baseline.as_ref() => {
            RunnerOutcome::completed(last)
        }
        // `turn` judges any normally settled turn as completion, even without assistant text
        // (upstream `completion === "turn"`); `error`/`aborted` stopReasons were handled above, so
        // a genuine failure still wins.
        _ if completion == ChildCompletionPolicy::Turn => RunnerOutcome::completed(""),
        _ => RunnerOutcome::error(
            RunnerFailureKind::ChildTurnFailed,
            observation
                .error_message
                .clone()
                .unwrap_or_else(|| "child turn produced no assistant output".to_string()),
        ),
    }
}

fn settled_session_outcome(
    session: &dyn ChildSession,
    completion: ChildCompletionPolicy,
) -> RunnerOutcome {
    match session.get_last_assistant_text() {
        Some(last) if !last.is_empty() => RunnerOutcome::completed(last),
        // A restored `turn` child drains as a completed empty turn instead of a no-output error.
        _ if completion == ChildCompletionPolicy::Turn => RunnerOutcome::completed(""),
        _ => RunnerOutcome::error(
            RunnerFailureKind::ChildTurnFailed,
            "restored session has no assistant output",
        ),
    }
}

/// The in-process child handle: each prompt runs on its own thread; `wait_for_idle` blocks until
/// the current turn settles. A follow-up while a turn runs queues on the session instead.
pub struct InProcessChildHandle {
    task_id: String,
    session: Arc<dyn ChildSession>,
    tracked: Arc<Tracked>,
    completion: ChildCompletionPolicy,
    native_pause: Option<super::native_run_pause::InProcessRunPause>,
    unsubscribe_observer: Mutex<Option<Unsubscribe>>,
}

impl InProcessChildHandle {
    fn tracked(
        task_id: &str,
        session: Arc<dyn ChildSession>,
        completion: ChildCompletionPolicy,
    ) -> Arc<Self> {
        let tracked = Arc::new(Tracked {
            state: Mutex::new(TrackedState {
                aborted: false,
                disposed: false,
                turn: 0,
                outcome: Some(settled_session_outcome(session.as_ref(), completion)),
                outcomes: std::collections::HashMap::new(),
                observation: TurnObservation::default(),
            }),
            settled: Condvar::new(),
        });
        let observer = Arc::clone(&tracked);
        let unsubscribe = session.subscribe(Arc::new(move |event| {
            observer.lock().observation.observe(event);
        }));
        let native_pause = session.hold_session().map(|hold| {
            let child = session.clone();
            super::native_run_pause::create_in_process_run_pause(
                hold,
                Arc::new(move || Self::watch_engine_turn(child.clone(), completion)),
            )
        });
        Arc::new(Self {
            native_pause,
            task_id: task_id.to_string(),
            session,
            tracked,
            completion,
            unsubscribe_observer: Mutex::new(Some(unsubscribe)),
        })
    }

    pub fn create_staged_child_handle(
        task_id: &str,
        session: Arc<dyn ChildSession>,
        completion: ChildCompletionPolicy,
    ) -> Arc<Self> {
        Self::tracked(task_id, session, completion)
    }

    pub fn watch_engine_turn(
        session: Arc<dyn ChildSession>,
        completion: ChildCompletionPolicy,
    ) -> super::native_run_pause::EngineTurnWatch {
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let sender = Arc::new(Mutex::new(Some(send)));
        let pending = sender.clone();
        let observation = Arc::new(Mutex::new(TurnObservation {
            baseline: session.get_last_assistant_text(),
            ..Default::default()
        }));
        let observed = observation.clone();
        let child = session.clone();
        let started = std::sync::atomic::AtomicBool::new(false);
        let unsubscribe = session.subscribe(Arc::new(move |event| {
            if event["type"] == "agent_start" {
                started.store(true, std::sync::atomic::Ordering::Release);
            }
            if !started.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            observed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .observe(event);
            if event["type"] == "agent_end" {
                let outcome = turn_outcome(
                    child.as_ref(),
                    &observed.lock().unwrap_or_else(PoisonError::into_inner),
                    completion,
                );
                if let Some(sender) = pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
                {
                    let _ = sender.send(outcome);
                }
            }
        }));
        let subscription = Arc::new(Mutex::new(Some(unsubscribe)));
        let cancel_subscription = subscription.clone();
        super::native_run_pause::EngineTurnWatch {
            outcome: Box::new(move || {
                let result = receive.recv().unwrap_or_else(|_| {
                    RunnerOutcome::error(
                        RunnerFailureKind::ChildTurnFailed,
                        "engine turn watch cancelled",
                    )
                });
                if let Some(unsubscribe) = subscription
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
                {
                    unsubscribe();
                }
                result
            }),
            cancel: Box::new(move || {
                sender.lock().unwrap_or_else(PoisonError::into_inner).take();
                if let Some(unsubscribe) = cancel_subscription
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
                {
                    unsubscribe();
                }
            }),
        }
    }

    /// `createChildHandle` with the DEFAULT `FinalText` policy; other callers keep this signature.
    pub fn start(task_id: &str, session: Arc<dyn ChildSession>, prompt_text: &str) -> Arc<Self> {
        Self::start_with_completion(
            task_id,
            session,
            prompt_text,
            ChildCompletionPolicy::FinalText,
        )
    }

    /// `createChildHandle`: start tracking and immediately run the initial prompt.
    pub fn start_with_completion(
        task_id: &str,
        session: Arc<dyn ChildSession>,
        prompt_text: &str,
        completion: ChildCompletionPolicy,
    ) -> Arc<Self> {
        let handle = Self::tracked(task_id, session, completion);
        handle.begin_turn(prompt_text);
        handle
    }

    /// `createRestoredChildHandle` with the DEFAULT `FinalText` policy; other callers keep this
    /// signature.
    pub fn restored(task_id: &str, session: Arc<dyn ChildSession>) -> Arc<Self> {
        Self::restored_with_completion(task_id, session, ChildCompletionPolicy::FinalText)
    }

    /// `createRestoredChildHandle`: no prompt is replayed; idle drains the transcript outcome.
    pub fn restored_with_completion(
        task_id: &str,
        session: Arc<dyn ChildSession>,
        completion: ChildCompletionPolicy,
    ) -> Arc<Self> {
        let handle = Self::tracked(task_id, session, completion);
        handle.session.park_idle_memory();
        handle
    }

    pub(crate) fn begin_turn(&self, text: &str) -> crate::pause::ports::ResumedChildTurn {
        // The host snapshots its transcript to answer this, so read it before taking the lock the
        // session event observer also needs; no turn is active here, so no event can race it.
        let baseline =
            self.session
                .get_last_assistant_text()
                .or_else(|| match &self.tracked.lock().outcome {
                    Some(RunnerOutcome::Completed { final_response })
                        if !final_response.is_empty() =>
                    {
                        Some(final_response.clone())
                    }
                    _ => None,
                });
        let turn = {
            let mut state = self.tracked.lock();
            state.aborted = false;
            state.turn += 1;
            state.outcome = None;
            state.observation = TurnObservation {
                baseline,
                ..TurnObservation::default()
            };
            state.turn
        };
        let session = Arc::clone(&self.session);
        let tracked = Arc::clone(&self.tracked);
        let completion = self.completion;
        let text = text.to_string();
        let exact_tracked = Arc::clone(&tracked);
        thread::spawn(move || {
            let prompted = session.prompt(&text);
            let outcome = {
                let mut state = tracked.lock();
                if state.turn != turn {
                    state.outcomes.insert(turn, RunnerOutcome::Cancelled);
                    tracked.settled.notify_all();
                    return;
                }
                let outcome = match prompted {
                    _ if state.aborted => RunnerOutcome::Cancelled,
                    Err(error) => {
                        RunnerOutcome::error(RunnerFailureKind::ChildPromptFailed, error.message)
                    }
                    Ok(()) => turn_outcome(session.as_ref(), &state.observation, completion),
                };
                state.observation = TurnObservation::default();
                outcome
            };
            session.park_idle_memory();
            let mut state = tracked.lock();
            if state.turn != turn {
                state.outcomes.insert(turn, outcome);
                tracked.settled.notify_all();
                return;
            }
            state.outcome = Some(if state.aborted {
                RunnerOutcome::Cancelled
            } else {
                outcome
            });
            let outcome = state.outcome.clone().expect("settled turn");
            state.outcomes.insert(turn, outcome);
            tracked.settled.notify_all();
        });
        crate::pause::ports::ResumedChildTurn {
            turn_id: turn.to_string(),
            outcome: Box::new(move || {
                let mut state = exact_tracked.lock();
                loop {
                    if let Some(outcome) = state.outcomes.remove(&turn) {
                        return outcome;
                    }
                    state = exact_tracked
                        .settled
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }),
        }
    }

    fn turn_active(&self) -> bool {
        self.tracked.lock().outcome.is_none()
    }

    pub fn session_id(&self) -> String {
        self.session.session_id()
    }

    pub fn wait_for_idle(&self) -> RunnerOutcome {
        let mut state = self.tracked.lock();
        loop {
            if let Some(outcome) = &state.outcome {
                return outcome.clone();
            }
            state = self
                .tracked
                .settled
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    pub fn follow_up_turn(&self, text: &str) -> Result<(), HostError> {
        if self.turn_active() {
            return self.session.follow_up(text);
        }
        self.begin_turn(text);
        Ok(())
    }

    pub fn abort_turn(&self) -> Result<(), HostError> {
        self.tracked.lock().aborted = true;
        self.session.abort()
    }

    pub fn dispose_handle(&self) {
        {
            let mut state = self.tracked.lock();
            if state.disposed {
                return;
            }
            state.disposed = true;
        }
        let unsubscribe = self
            .unsubscribe_observer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(unsubscribe) = unsubscribe {
            unsubscribe();
        }
        self.session.dispose();
    }
}

pub fn dispose_rejected_session(session: &dyn ChildSession) {
    session.dispose();
}

impl ManagedChildHandle for InProcessChildHandle {
    fn start_initial_turn(&self, text: &str) -> Option<crate::pause::ports::ResumedChildTurn> {
        Some(self.begin_turn(text))
    }
    fn run_pause(&self) -> Option<&dyn crate::pause::native_child_pause::NativeRunPause> {
        self.native_pause
            .as_ref()
            .map(|pause| pause as &dyn crate::pause::native_child_pause::NativeRunPause)
            .or_else(|| self.session.run_pause())
    }
    fn task_id(&self) -> &str {
        &self.task_id
    }

    fn session_id(&self) -> Option<String> {
        Some(self.session.session_id())
    }

    fn pid(&self) -> Option<i64> {
        None
    }

    fn steer(&self, text: &str) -> Result<(), HostError> {
        self.session.steer(text)
    }

    fn follow_up(&self, text: &str) -> Result<(), HostError> {
        self.follow_up_turn(text)
    }

    fn rebind_workpool_identity(
        &self,
        identity: crate::manager::child_handle::WorkpoolChildIdentity,
    ) -> Option<Result<(), HostError>> {
        self.session
            .rebind_workpool_identity(WorkpoolChildIdentity {
                task_id: identity.task_id,
                state_dir: identity.state_dir,
                run_epoch: identity.run_epoch,
            })
    }

    fn abort(&self) -> Result<(), HostError> {
        self.abort_turn()
    }

    fn subscribe(&self, listener: ManagedChildListener) -> Unsubscribe {
        self.session
            .subscribe(Arc::new(move |event| listener(&managed_event(event))))
    }

    fn wait_for_outcome(&self) -> RunnerOutcome {
        self.wait_for_idle()
    }

    fn last_assistant_text(&self) -> Option<String> {
        self.session
            .get_last_assistant_text()
            .or_else(|| match &self.tracked.lock().outcome {
                Some(RunnerOutcome::Completed { final_response }) if !final_response.is_empty() => {
                    Some(final_response.clone())
                }
                _ => None,
            })
    }

    fn dispose(&self) -> Result<(), HostError> {
        self.dispose_handle();
        Ok(())
    }
}
