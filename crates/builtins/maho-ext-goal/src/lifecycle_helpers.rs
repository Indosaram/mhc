use crate::{continuation::DenyReason,continuation_recovery::*,types::{Goal,GoalStatus}};
use maho_agent::types::AgentMessage;
use maho_ai::types::ContentBlock;
use maho_ext_api::{ExtensionContext,ExtensionFailure};

pub fn is_resume_of_stopped_goal(ctx:&ExtensionContext,reason:&str,goal:Option<&Goal>)->Result<bool,ExtensionFailure> {
    if reason!="resume" || !goal.is_some_and(|goal|matches!(goal.status,GoalStatus::Paused|GoalStatus::Blocked)) || !ctx.has_ui || !ctx.is_idle() { return Ok(false); }
    Ok(!ctx.has_pending_messages()?)
}
pub fn last_assistant_text(messages:&[AgentMessage])->String {
    crate::last_assistant_message::last_assistant_message(messages).map_or_else(String::new,|message|message.content.iter().filter_map(|block|match block { ContentBlock::Text(text)=>Some(text.text.as_str()),_=>None }).collect::<Vec<_>>().join("\n"))
}
pub fn blocked_reason_for_continuation_guard(reason:DenyReason)->Option<&'static str> {
    match reason {
        DenyReason::Cap=>Some(CONTINUATION_CAP_BLOCKED_REASON),
        DenyReason::Unattended=>Some(UNATTENDED_CONTINUATION_BLOCKED_REASON),
        DenyReason::Repetition=>Some(REPETITION_BLOCKED_REASON),
        DenyReason::LengthExhausted=>Some(LENGTH_EXHAUSTED_BLOCKED_REASON),
        DenyReason::ContextOverflow=>Some(CONTEXT_OVERFLOW_BLOCKED_REASON),
        DenyReason::NotEligible|DenyReason::SingleFlight|DenyReason::Stale=>None,
    }
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn only_guardrail_denials_block_persisted_goal() {
        for reason in [DenyReason::Cap,DenyReason::Unattended,DenyReason::Repetition,DenyReason::LengthExhausted,DenyReason::ContextOverflow] { assert!(is_mechanical_continuation_block(blocked_reason_for_continuation_guard(reason))); }
        for reason in [DenyReason::NotEligible,DenyReason::SingleFlight,DenyReason::Stale] { assert_eq!(blocked_reason_for_continuation_guard(reason),None); }
    }
    #[test] fn absent_assistant_has_no_text() { assert!(last_assistant_text(&[]).is_empty()); }
}
