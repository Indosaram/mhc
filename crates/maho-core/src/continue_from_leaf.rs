//! Port of senpi `packages/coding-agent/src/core/continue-from-leaf.ts`.
//!
//! Continue a session from its current leaf with no new prompt (senpi #1930). A request may not end
//! with an assistant message on the default models, so the continuation is a hidden custom message
//! (display false) that drives the next turn, like the "." manual-continue shortcut. It has its own
//! type so extensions that react to a manual continue do not mistake it for one.

use std::sync::{Arc, Mutex};

pub const CONTINUE_FROM_LEAF_CUSTOM_TYPE: &str = "continue-from-leaf";

pub const CONTINUE_FROM_LEAF_DIRECTIVE: &str = "<system-notice>
Continue from where the conversation ends.

Your last reply may have been edited by the user; treat its current text as your own words.
Carry on from it without repeating it, apologizing for it, or commenting on the edit.
</system-notice>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContinueFromLeafCode {
    Streaming,
    NothingToContinue,
    LeafNotAssistant,
}

impl ContinueFromLeafCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Streaming => "streaming",
            Self::NothingToContinue => "nothing_to_continue",
            Self::LeafNotAssistant => "leaf_not_assistant",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::Streaming => "Wait for the current response to finish before continuing.",
            Self::NothingToContinue => {
                "There is nothing to continue: the session has no messages yet."
            }
            Self::LeafNotAssistant => {
                "There is no answer to continue: the conversation ends on a user message. Send it, or retry it, instead."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContinueFromLeafError {
    pub code: ContinueFromLeafCode,
}

impl std::fmt::Display for ContinueFromLeafError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code.message())
    }
}

impl std::error::Error for ContinueFromLeafError {}

impl ContinueFromLeafError {
    pub const NAME: &'static str = "ContinueFromLeafError";

    pub fn new(code: ContinueFromLeafCode) -> Self {
        Self { code }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnAdmissionDisposition {
    Started,
    Delegated,
    FinishedWithoutStart,
}

#[derive(Default)]
struct AdmissionState {
    agent_start_seen: bool,
    start_disposition_seen: bool,
    resolved: bool,
}

/// `trackTurnAdmission`: resolves once the runtime took a continuation, its turn started
/// (`agent_start` plus the `started` disposition) or it was queued into a running turn (`delegated`).
/// The two signals can arrive in either order, so the tracker resolves on the pair (senpi #2708).
#[derive(Clone)]
pub struct TurnAdmissionTracker {
    state: Arc<Mutex<AdmissionState>>,
    started: Arc<tokio::sync::Notify>,
}

impl Default for TurnAdmissionTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl TurnAdmissionTracker {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(AdmissionState::default())),
            started: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn update(&self, apply: impl FnOnce(&mut AdmissionState) -> bool) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.resolved {
            return;
        }
        if apply(&mut state) {
            state.resolved = true;
            self.started.notify_waiters();
        }
    }

    /// The disposition the runtime reported for the continuation.
    pub fn disposition(&self, disposition: TurnAdmissionDisposition) {
        self.update(|state| match disposition {
            TurnAdmissionDisposition::Delegated => true,
            TurnAdmissionDisposition::Started => {
                state.start_disposition_seen = true;
                state.agent_start_seen
            }
            TurnAdmissionDisposition::FinishedWithoutStart => false,
        });
    }

    /// An `agent_start` event from the session's subscription.
    pub fn agent_start(&self) {
        self.update(|state| {
            state.agent_start_seen = true;
            state.start_disposition_seen
        });
    }

    pub fn is_started(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.resolved)
            .unwrap_or(false)
    }

    /// Resolves once the continuation was admitted.
    pub async fn wait(&self) {
        loop {
            let notified = self.started.notified();
            if self.is_started() {
                return;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_messages_match_the_pinned_source() {
        assert_eq!(
            ContinueFromLeafError::new(ContinueFromLeafCode::Streaming).to_string(),
            "Wait for the current response to finish before continuing."
        );
        assert_eq!(
            ContinueFromLeafError::new(ContinueFromLeafCode::LeafNotAssistant).to_string(),
            "There is no answer to continue: the conversation ends on a user message. Send it, or retry it, instead."
        );
        assert_eq!(
            ContinueFromLeafCode::NothingToContinue.as_str(),
            "nothing_to_continue"
        );
    }

    #[test]
    fn admission_resolves_on_the_pair_in_either_order() {
        let early = TurnAdmissionTracker::new();
        early.agent_start();
        assert!(!early.is_started());
        early.disposition(TurnAdmissionDisposition::Started);
        assert!(early.is_started());

        let late = TurnAdmissionTracker::new();
        late.disposition(TurnAdmissionDisposition::Started);
        assert!(!late.is_started());
        late.agent_start();
        assert!(late.is_started());
    }

    #[test]
    fn delegation_resolves_alone_and_finishing_without_start_never_does() {
        let delegated = TurnAdmissionTracker::new();
        delegated.disposition(TurnAdmissionDisposition::Delegated);
        assert!(delegated.is_started());

        let finished = TurnAdmissionTracker::new();
        finished.disposition(TurnAdmissionDisposition::FinishedWithoutStart);
        finished.agent_start();
        assert!(!finished.is_started());
    }

    #[tokio::test]
    async fn wait_returns_once_admitted() {
        let tracker = TurnAdmissionTracker::new();
        let waiter = tracker.clone();
        let handle = tokio::spawn(async move { waiter.wait().await });
        tracker.disposition(TurnAdmissionDisposition::Delegated);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), handle)
                .await
                .is_ok()
        );
    }
}
