//! Lifecycle decisions: what to inject on a prompt, a turn start, a compaction and a todo result.

use crate::classify::{Classification, ClassifyInput, Stage, classify_with_session_role};
use crate::state::{PendingDirective, Sessions};
use crate::texts::{directive_message, inline_block, reminder_message, todo_advisory};
use maho_ext_api::{CustomMessage, InputSource};
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug, Clone, Copy)]
pub struct InputRequest<'a> {
    pub session: &'a str,
    pub text: &'a str,
    pub source: InputSource,
    /// The input arrives while a turn streams, so it can only carry text.
    pub streaming: bool,
}

#[derive(Debug, Clone)]
pub struct InputOutcome {
    pub classification: Classification,
    /// Prompt text with the block appended, for inputs that cannot carry a custom message.
    pub transformed_text: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct TodoResultView<'a> {
    pub session: &'a str,
    pub tool_name: &'a str,
    pub is_error: bool,
    pub details: Option<&'a Value>,
}

#[derive(Clone)]
pub struct Engine {
    sessions: Arc<Sessions>,
}

impl Default for Engine {
    /// An engine over the process-wide session table.
    fn default() -> Self {
        Self::new(crate::global_sessions())
    }
}

impl Engine {
    pub fn new(sessions: Arc<Sessions>) -> Self {
        Self { sessions }
    }

    pub fn sessions(&self) -> &Sessions {
        &self.sessions
    }

    pub fn on_input(&self, request: InputRequest<'_>, disabled: bool) -> InputOutcome {
        self.on_input_with_session_role(request, disabled, None)
    }

    pub fn on_input_with_session_role(
        &self,
        request: InputRequest<'_>,
        disabled: bool,
        session_role: Option<&str>,
    ) -> InputOutcome {
        let snapshot = self.sessions.snapshot(request.session);
        let classification = classify_with_session_role(
            ClassifyInput {
                text: request.text,
                source: request.source,
                snapshot,
                disabled,
            },
            session_role,
        );
        let mut outcome = InputOutcome {
            classification,
            transformed_text: None,
        };
        if disabled {
            return outcome;
        }
        if classification.already_delivered {
            self.sessions.mark_armed(request.session);
            return outcome;
        }
        if !classification.effective {
            return outcome;
        }
        let full = matches!(classification.stage, Stage::Initial | Stage::CompactRearm);
        self.sessions.mark_armed(request.session);
        if request.streaming {
            let block = inline_block(full, classification.mass);
            outcome.transformed_text = Some(format!("{}\n\n{block}", request.text));
        } else {
            self.sessions.set_pending(
                request.session,
                PendingDirective {
                    full,
                    mass: classification.mass,
                },
            );
        }
        outcome
    }

    /// The hidden message for the turn that is about to start. A preview leaves the state alone.
    pub fn on_turn_start(&self, session: &str, preview: bool) -> Option<CustomMessage> {
        let found = if preview {
            self.sessions.pending(session)
        } else {
            self.sessions.take_pending(session)
        };
        let pending = found?;
        Some(if pending.full {
            directive_message(crate::contract_revision(), pending.mass)
        } else {
            reminder_message(pending.mass)
        })
    }

    pub fn on_compaction(&self, session: &str) {
        let arming = self.sessions.snapshot(session);
        if arming.armed || arming.compaction_pending {
            self.sessions.mark_compact_rearm(session);
        }
    }

    /// The advisory text to append to this tool result, if the todo trigger conditions hold.
    pub fn on_tool_result(&self, view: TodoResultView<'_>, disabled: bool) -> Option<&'static str> {
        if disabled || view.tool_name != "todo" || view.is_error {
            return None;
        }
        let details = view.details?;
        let snapshot = self.sessions.snapshot(view.session);
        // After a compaction the session counts as armed until the next mention re-arms it.
        let armed = snapshot.armed || snapshot.compaction_pending;
        if !matches!(details.get("op").and_then(Value::as_str), Some("init" | "append"))
            || !armed
        {
            return None;
        }
        self.sessions
            .latch_todo_reminder(view.session)
            .then(todo_advisory)
    }
}
