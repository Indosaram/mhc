//! Port of senpi `packages/coding-agent/src/modes/rpc/mcp-presenter-form-group.ts`.
//!
//! One form offered to every holder current when it reached the head of its binding's FIFO
//! (D11 S3-3b) with a single synchronous terminal latch.

use crate::mcp_form_dialog::McpFormClock;
use crate::mcp_form_types::{McpFormOutcome, McpFormOutcomeKind};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupTermination {
    PresentationLost,
    PresentationTimeout,
    OperationAborted,
}

pub fn reason_of_outcome(outcome: &McpFormOutcome) -> &'static str {
    match &outcome.kind {
        McpFormOutcomeKind::Result(result) => {
            match result.get("action").and_then(serde_json::Value::as_str) {
                Some("accept") => "answered",
                Some("decline") => "declined",
                _ => "cancelled",
            }
        }
        McpFormOutcomeKind::OperationAborted => "operation_aborted",
        McpFormOutcomeKind::ProcessingFailure => "processing_failed",
    }
}

struct GroupState {
    settled: bool,
    presented: bool,
    members: HashSet<String>,
    resolver: Option<oneshot::Sender<McpFormOutcome>>,
}

pub struct McpFormGroup {
    pub id: String,
    pub session_id: String,
    deadline_ms: f64,
    clock: Arc<dyn McpFormClock>,
    admit: Arc<dyn Fn() + Send + Sync>,
    state: Mutex<GroupState>,
    timer: Mutex<Option<u64>>,
}

impl McpFormGroup {
    pub fn new(
        session_id: impl Into<String>,
        deadline_ms: f64,
        clock: Arc<dyn McpFormClock>,
        admit: Arc<dyn Fn() + Send + Sync>,
    ) -> (Self, oneshot::Receiver<McpFormOutcome>) {
        let (tx, rx) = oneshot::channel();
        let now = clock.now();
        (
            Self {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: session_id.into(),
                deadline_ms: now + deadline_ms.max(0.0),
                clock,
                admit,
                state: Mutex::new(GroupState {
                    settled: false,
                    presented: false,
                    members: HashSet::new(),
                    resolver: Some(tx),
                }),
                timer: Mutex::new(None),
            },
            rx,
        )
    }

    pub fn join_member(&self, dialog_id: impl Into<String>) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .members
            .insert(dialog_id.into());
    }

    pub fn mark_presented(&self) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .presented = true;
    }

    pub fn leave_member(&self, dialog_id: &str) -> bool {
        let empty = {
            let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            st.members.remove(dialog_id);
            !st.settled && st.members.is_empty()
        };
        if empty {
            self.terminate(GroupTermination::PresentationLost);
        }
        empty
    }

    pub fn arm_timer(self: &Arc<Self>, delay_ms: f64) {
        let weak = Arc::downgrade(self);
        let id = self.clock.schedule(
            Box::new(move || {
                if let Some(group) = weak.upgrade() {
                    group.terminate(GroupTermination::PresentationTimeout);
                }
            }),
            delay_ms,
        );
        *self.timer.lock().unwrap_or_else(PoisonError::into_inner) = Some(id);
    }

    pub fn live(&self, operation_aborted: bool) -> bool {
        if operation_aborted {
            self.terminate(GroupTermination::OperationAborted);
            return false;
        }
        if self.clock.now() >= self.deadline_ms {
            self.terminate(GroupTermination::PresentationTimeout);
            return false;
        }
        true
    }

    pub fn terminate(&self, cause: GroupTermination) {
        let presented = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .presented;
        let outcome = if cause == GroupTermination::OperationAborted {
            McpFormOutcome::operation_aborted()
        } else if presented {
            McpFormOutcome::terminated(McpFormOutcomeKind::Result(serde_json::json!({
                "action": "cancel"
            })))
        } else {
            McpFormOutcome::processing_failure()
        };
        self.settle(outcome);
    }

    pub fn settle(&self, outcome: McpFormOutcome) -> bool {
        let (resolver, human_admission) = {
            let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if st.settled {
                return false;
            }
            st.settled = true;
            st.members.clear();
            (st.resolver.take(), outcome.human_admission())
        };
        if let Some(timer_id) = self
            .timer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            self.clock.cancel(timer_id);
        }
        if human_admission {
            (self.admit)();
        }
        if let Some(tx) = resolver {
            let _ = tx.send(outcome);
        }
        true
    }
}
