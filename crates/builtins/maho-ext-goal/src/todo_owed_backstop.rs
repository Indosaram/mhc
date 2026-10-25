use crate::types::{Goal, GoalStatus};
use maho_core::agent_abort_provenance::AgentEndEvent;
use maho_ext_api::{ExtensionApi, ExtensionContext, ExtensionFailure};
use std::collections::BTreeSet;
pub const TODO_OWED_CUSTOM_TYPE: &str = "senpi.todo-owed";
pub const TODO_OWED_REMINDER_EVENT: &str = "todo_owed_reminder";
pub const TODO_OWED_CHAIN_LIMIT: usize = 2;
pub fn ends_with_question(text: &str) -> bool {
    let paragraphs = regex::Regex::new(r"\n\s*\n").expect("static paragraph pattern");
    let last = paragraphs
        .split(text)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .last()
        .unwrap_or("");
    regex::Regex::new(r#"[?？]["'`*_)\]}’”]*(?:\s|$)"#)
        .expect("static question pattern")
        .is_match(last)
}
pub fn called_ask_user(messages: &[maho_agent::types::AgentMessage]) -> bool {
    messages.iter().any(|message|match message {
        maho_agent::types::AgentMessage::Llm(maho_ai::types::Message::Assistant(message))=>message.content.iter().any(|part|matches!(part,maho_ai::types::ContentBlock::ToolCall(call) if matches!(call.name.as_str(),"ask_user_question"|"request_user_input"|"ask-user"))), _=>false,
    })
}
#[derive(Default)]
pub struct TodoOwedBackstop {
    reminders: usize,
    notified: bool,
    previous: Option<BTreeSet<String>>,
}
impl TodoOwedBackstop {
    pub fn reset_chain(&mut self) {
        *self = Self::default();
    }
    pub fn after_agent_end(
        &mut self,
        pi: &ExtensionApi,
        ctx: &ExtensionContext,
        event: &AgentEndEvent,
        goal: Option<&Goal>,
        pending: bool,
        sources: &[String],
        enabled: bool,
    ) -> Result<String, ExtensionFailure> {
        let open = crate::todo_gate::open_todo_task_contents(&ctx.session_manager.get_branch());
        let suppression = if matches!(
            ctx.mode,
            maho_ext_api::ExtensionMode::Print | maho_ext_api::ExtensionMode::Json
        ) {
            Some("non-interactive")
        } else if event.aborted || !crate::continuation::did_agent_end_cleanly(&event.messages) {
            Some("not-clean")
        } else if crate::last_assistant_message::last_assistant_message(&event.messages)
            .is_some_and(|message| message.stop_reason == maho_ai::types::StopReason::Length)
        {
            Some("length")
        } else if open.is_empty() {
            Some("no-open-tasks")
        } else if !sources.is_empty()
            || pending
            || ctx.has_pending_messages()?
            || goal.is_some_and(|goal| {
                goal.status == GoalStatus::Active && goal.continuation_stopped_at.is_none()
            })
        {
            Some("waiting")
        } else if goal.is_some_and(|goal| {
            goal.status == GoalStatus::Active && goal.continuation_stopped_at.is_some()
        }) {
            Some("goal-stopped")
        } else if called_ask_user(&event.messages) {
            Some("ask-user-call")
        } else if ends_with_question(&crate::lifecycle_helpers::last_assistant_text(
            &event.messages,
        )) {
            Some("question")
        } else if !enabled {
            Some("disabled")
        } else {
            None
        };
        if let Some(reason) = suppression {
            return Ok(reason.into());
        }
        let current: BTreeSet<_> = open.iter().cloned().collect();
        if self
            .previous
            .as_ref()
            .is_some_and(|previous| previous.iter().any(|task| !current.contains(task)))
        {
            self.reset_chain();
        }
        let outcome = if self.reminders >= TODO_OWED_CHAIN_LIMIT {
            if self.notified {
                return Ok("cap".into());
            }
            self.notified = true;
            ctx.ui.notify(&format!("Agent stopped with {} open todo tasks (Now: {}): {} automatic continuations in a row ended without closing a task. Reply to resume it, or close the work with /todo done or /todo drop.",open.len(),open[0],self.reminders),maho_ext_api::NotificationType::Warning);
            "capped"
        } else {
            self.reminders += 1;
            self.previous = Some(current);
            let content = format!(
                "<system-reminder>\n{}The turn ended with open todo work and no question to the user. Now: {}. Next: {}. Either continue the next open task now with real tool calls, or mark it done/dropped via the todo tool and tell the user in one sentence why you stopped. Do not restate the plan.\n</system-reminder>",
                if self.reminders == TODO_OWED_CHAIN_LIMIT {
                    "Second and final reminder. "
                } else {
                    ""
                },
                open[0],
                open.get(1).unwrap_or(&open[0])
            );
            pi.send_message(
                maho_ext_api::CustomMessage {
                    custom_type: TODO_OWED_CUSTOM_TYPE.into(),
                    content: vec![maho_ext_api::ToolContent::text(content)],
                    display: false,
                    details: None,
                },
                maho_ext_api::SendMessageOptions {
                    trigger_turn: true,
                    deliver_as: Some(maho_ext_api::DeliverAs::FollowUp),
                },
            )?;
            "delivered"
        };
        pi.events.emit(TODO_OWED_REMINDER_EVENT,&serde_json::json!({"sessionId":ctx.session_manager.session_id(),"chainCount":self.reminders,"openTasks":open.len(),"now":open[0],"next":open.get(1).unwrap_or(&open[0]),"reason":outcome}));
        Ok(outcome.into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn question_only_in_final_paragraph() {
        assert!(ends_with_question("Done.\n\nContinue? Yes or no."));
        assert!(!ends_with_question("Continue?\n\nDone."));
        assert!(ends_with_question("What？”"));
    }
}
