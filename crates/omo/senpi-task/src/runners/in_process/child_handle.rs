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

/// Host session events are untrusted JSON (`{ type, message? }`).
pub type ChildSessionListener = Arc<dyn Fn(&Value) + Send + Sync>;

/// The host agent-session surface an in-process child drives. `prompt` blocks until the turn ends.
pub trait ChildSession: Send + Sync {
    fn session_id(&self) -> String;
    fn prompt(&self, text: &str) -> Result<(), HostError>;
    fn steer(&self, text: &str) -> Result<(), HostError>;
    fn follow_up(&self, text: &str) -> Result<(), HostError>;
    fn abort(&self) -> Result<(), HostError>;
    fn subscribe(&self, listener: ChildSessionListener) -> Unsubscribe;
    fn get_last_assistant_text(&self) -> Option<String>;
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

fn turn_outcome(session: &dyn ChildSession, observation: &TurnObservation) -> RunnerOutcome {
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
        _ => RunnerOutcome::error(
            RunnerFailureKind::ChildTurnFailed,
            observation
                .error_message
                .clone()
                .unwrap_or_else(|| "child turn produced no assistant output".to_string()),
        ),
    }
}

fn settled_session_outcome(session: &dyn ChildSession) -> RunnerOutcome {
    match session.get_last_assistant_text() {
        Some(last) if !last.is_empty() => RunnerOutcome::completed(last),
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
    unsubscribe_observer: Mutex<Option<Unsubscribe>>,
}

impl InProcessChildHandle {
    fn tracked(task_id: &str, session: Arc<dyn ChildSession>) -> Arc<Self> {
        let tracked = Arc::new(Tracked {
            state: Mutex::new(TrackedState {
                aborted: false,
                disposed: false,
                turn: 0,
                outcome: Some(settled_session_outcome(session.as_ref())),
                observation: TurnObservation::default(),
            }),
            settled: Condvar::new(),
        });
        let observer = Arc::clone(&tracked);
        let unsubscribe = session.subscribe(Arc::new(move |event| {
            observer.lock().observation.observe(event);
        }));
        Arc::new(Self {
            task_id: task_id.to_string(),
            session,
            tracked,
            unsubscribe_observer: Mutex::new(Some(unsubscribe)),
        })
    }

    /// `createChildHandle`: start tracking and immediately run the initial prompt.
    pub fn start(task_id: &str, session: Arc<dyn ChildSession>, prompt_text: &str) -> Arc<Self> {
        let handle = Self::tracked(task_id, session);
        handle.begin_turn(prompt_text);
        handle
    }

    /// `createRestoredChildHandle`: no prompt is replayed; idle drains the transcript outcome.
    pub fn restored(task_id: &str, session: Arc<dyn ChildSession>) -> Arc<Self> {
        Self::tracked(task_id, session)
    }

    fn begin_turn(&self, text: &str) {
        let turn = {
            let mut state = self.tracked.lock();
            state.aborted = false;
            state.turn += 1;
            state.outcome = None;
            state.observation = TurnObservation {
                baseline: self.session.get_last_assistant_text(),
                ..TurnObservation::default()
            };
            state.turn
        };
        let session = Arc::clone(&self.session);
        let tracked = Arc::clone(&self.tracked);
        let text = text.to_string();
        thread::spawn(move || {
            let prompted = session.prompt(&text);
            let mut state = tracked.lock();
            if state.turn != turn {
                return;
            }
            let outcome = match prompted {
                _ if state.aborted => RunnerOutcome::Cancelled,
                Err(error) => {
                    RunnerOutcome::error(RunnerFailureKind::ChildPromptFailed, error.message)
                }
                Ok(()) => turn_outcome(session.as_ref(), &state.observation),
            };
            state.outcome = Some(outcome);
            tracked.settled.notify_all();
        });
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

impl ManagedChildHandle for InProcessChildHandle {
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
        self.session.get_last_assistant_text()
    }

    fn dispose(&self) -> Result<(), HostError> {
        self.dispose_handle();
        Ok(())
    }
}
