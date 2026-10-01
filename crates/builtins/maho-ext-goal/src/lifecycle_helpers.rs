use crate::{continuation::DenyReason,continuation_recovery::*,types::{Goal,GoalStatus}};
use maho_agent::types::AgentMessage;
use maho_ai::types::ContentBlock;
use maho_ext_api::{ExtensionContext,ExtensionFailure};

pub fn queue_hidden_goal_prompt(api:&maho_ext_api::ExtensionApi,content:String)->Result<(),ExtensionFailure> {
    api.send_message(maho_ext_api::CustomMessage {
        custom_type:"goal-continuation".into(),content:vec![maho_ext_api::ToolContent::text(content)],display:false,details:None,
    },maho_ext_api::SendMessageOptions { trigger_turn:true,deliver_as:Some(maho_ext_api::DeliverAs::FollowUp) })
}

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
    #[test] fn hidden_prompt_uses_bound_followup_transport() {
        use maho_ext_api::*;
        struct Capture(std::sync::Mutex<Option<(CustomMessage,SendMessageOptions)>>);
        impl ExtensionActions for Capture {
            fn send_message(&self,message:CustomMessage,options:SendMessageOptions)->Result<(),ExtensionFailure> { *self.0.lock().unwrap()=Some((message,options)); Ok(()) }
            fn send_user_message(&self,_:UserMessageContent,_:SendUserMessageOptions)->Result<(),ExtensionFailure> { Err("unexpected user message".into()) }
            fn append_entry(&self,_:&str,_:Option<JsonValue>)->Result<(),ExtensionFailure> { Err("unexpected entry".into()) }
            fn get_all_tools(&self)->Result<Vec<ToolInfo>,ExtensionFailure> { Ok(Vec::new()) }
        }
        let capture=std::sync::Arc::new(Capture(std::sync::Mutex::new(None)));
        let runtime=ExtensionRuntime::default(); runtime.bind(capture.clone());
        let api=ExtensionApi::new(LoadedExtension::new("goal","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);
        queue_hidden_goal_prompt(&api,"payload".into()).unwrap();
        let (message,options)=capture.0.lock().unwrap().take().unwrap();
        assert_eq!(message.custom_type,"goal-continuation"); assert!(!message.display); assert!(options.trigger_turn); assert_eq!(options.deliver_as,Some(DeliverAs::FollowUp));
        assert_eq!(message.content,vec![ToolContent::text("payload")]);
    }
    #[test] fn only_guardrail_denials_block_persisted_goal() {
        for reason in [DenyReason::Cap,DenyReason::Unattended,DenyReason::Repetition,DenyReason::LengthExhausted,DenyReason::ContextOverflow] { assert!(is_mechanical_continuation_block(blocked_reason_for_continuation_guard(reason))); }
        for reason in [DenyReason::NotEligible,DenyReason::SingleFlight,DenyReason::Stale] { assert_eq!(blocked_reason_for_continuation_guard(reason),None); }
    }
    #[test] fn absent_assistant_has_no_text() { assert!(last_assistant_text(&[]).is_empty()); }
}
