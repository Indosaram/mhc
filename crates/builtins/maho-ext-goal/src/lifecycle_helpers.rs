use crate::{continuation::DenyReason,continuation_recovery::*,types::{Goal,GoalStatus}};
use maho_agent::types::AgentMessage;
use maho_ai::types::ContentBlock;
use maho_ext_api::{ExtensionContext,ExtensionFailure};

pub fn queue_hidden_goal_prompt(api:&maho_ext_api::ExtensionApi,content:String)->Result<(),ExtensionFailure> {
    api.send_message(maho_ext_api::CustomMessage {
        custom_type:"goal-continuation".into(),content:vec![maho_ext_api::ToolContent::text(content)],display:false,details:None,
    },maho_ext_api::SendMessageOptions { trigger_turn:true,deliver_as:Some(maho_ext_api::DeliverAs::FollowUp) })
}
pub async fn admit_and_record_goal_continuation(reference:&crate::types::GoalStoreRef,input:&crate::continuation::GoalContinuationInput<'_>,now:u64)->Result<(Option<Goal>,crate::continuation::GoalContinuationVerdict),ExtensionFailure> {
    use crate::continuation::{evaluate_goal_continuation,GoalContinuationVerdict,GoalContinuationPath};
    let verdict=evaluate_goal_continuation(input);
    let Some(goal)=input.goal else { return Ok((None,verdict)); };
    let recorded=match verdict {
        GoalContinuationVerdict::Deny(reason)=>{
            if let Some(reason)=blocked_reason_for_continuation_guard(reason) {
                Some(crate::store::update_goal(reference,&crate::types::GoalUpdate { status:Some(GoalStatus::Blocked),reason:Some(reason.into()),..Default::default() },crate::types::GoalUpdateSource::Model,now).await.map_err(|error|ExtensionFailure::new(error.to_string()))?)
            } else { Some(goal.clone()) }
        },
        GoalContinuationVerdict::Continue { .. }=>{
            let signature=input.current_signature.ok_or_else(||ExtensionFailure::new("Cannot queue a goal continuation without a progress signature"))?;
            crate::store::record_continuation_delivered(reference,signature,Some(&goal.id),input.path!=GoalContinuationPath::MonitorDelayed).await.map_err(|error|ExtensionFailure::new(error.to_string()))?
        },
    };
    Ok((recorded,verdict))
}
pub async fn admit_and_queue_goal_continuation(api:&maho_ext_api::ExtensionApi,reference:&crate::types::GoalStoreRef,input:&crate::continuation::GoalContinuationInput<'_>,now:u64,mark_pending:impl FnOnce(),content:impl FnOnce(crate::continuation::GoalContinuationVerdict)->String)->Result<Option<Goal>,ExtensionFailure> {
    let (recorded,verdict)=admit_and_record_goal_continuation(reference,input,now).await?;
    if recorded.is_some()&&matches!(verdict,crate::continuation::GoalContinuationVerdict::Continue { .. }) { mark_pending(); queue_hidden_goal_prompt(api,content(verdict))?; }
    Ok(recorded)
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
pub fn report_denied_continuation(api:&maho_ext_api::ExtensionApi,ctx:&ExtensionContext,goal:&Goal,input:&crate::continuation::GoalContinuationInput<'_>,reason:DenyReason) {
    let Some(blocked_reason)=blocked_reason_for_continuation_guard(reason) else { return; };
    if ctx.has_ui { ctx.ui.notify(&continuation_cap_recovery_hint(blocked_reason),maho_ext_api::NotificationType::Warning); }
    let reason=match reason { DenyReason::Cap=>"cap",DenyReason::Unattended=>"unattended",DenyReason::Repetition=>"repetition",DenyReason::LengthExhausted=>"length-exhausted",DenyReason::ContextOverflow=>"context-overflow",DenyReason::NotEligible|DenyReason::SingleFlight|DenyReason::Stale=>return };
    api.events.emit("goal_continuation_guard_tripped",&serde_json::json!({"goalId":goal.id,"reason":reason,"count":input.consecutive_continuations,"unattendedContinuations":goal.unattended_continuations.unwrap_or(0)}));
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn guard_trip_reports_machine_reason_and_counts_only_for_guardrail_denial() {
        use maho_ext_api::*;
        let goal:Goal=serde_json::from_value(serde_json::json!({"id":"g","threadId":"s","objective":"work","status":"blocked","tokensUsed":0,"timeUsedSeconds":0,"createdAt":0,"updatedAt":0})).unwrap();
        let api=ExtensionApi::new(LoadedExtension::new("goal","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
        let seen=std::sync::Arc::new(std::sync::Mutex::new(Vec::new())); let captured=seen.clone();
        let _subscription=api.events.on("goal_continuation_guard_tripped",std::sync::Arc::new(move |data|captured.lock().unwrap().push(data.clone())));
        let input=admission(&goal); let ctx=crate::test_context::context();
        report_denied_continuation(&api,&ctx,&goal,&input,DenyReason::SingleFlight); assert!(seen.lock().unwrap().is_empty());
        report_denied_continuation(&api,&ctx,&goal,&input,DenyReason::Cap);
        assert_eq!(*seen.lock().unwrap(),vec![serde_json::json!({"goalId":"g","reason":"cap","count":0,"unattendedContinuations":0})]);
    }
    fn admission(goal:&Goal)->crate::continuation::GoalContinuationInput<'_> {
        crate::continuation::GoalContinuationInput { goal:Some(goal),is_idle:true,has_pending_messages:false,path:crate::continuation::GoalContinuationPath::SessionStart,last_stop_reason:None,last_turn_was_malformed_tool_use:false,consecutive_continuations:0,last_continuation_signature:None,current_signature:Some("sig"),consecutive_length_recoveries:0,recent_normalized_output_hashes:&[],toolless_continuation_streak:0,continuation_pending:false,last_turn_stuck_on_context_overflow:false }
    }
    #[tokio::test] async fn admitted_delivery_records_before_transport_and_exempts_monitor_waits() {
        let dir=tempfile::tempdir().unwrap(); let reference=crate::types::GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() };
        let goal=crate::store::create_goal(&reference,"work",None,0).await.unwrap();
        let mut input=admission(&goal); input.path=crate::continuation::GoalContinuationPath::MonitorDelayed;
        let (recorded,verdict)=admit_and_record_goal_continuation(&reference,&input,1).await.unwrap();
        assert!(matches!(verdict,crate::continuation::GoalContinuationVerdict::Continue { .. }));
        let recorded=recorded.unwrap(); assert_eq!(recorded.consecutive_continuations,Some(1)); assert_eq!(recorded.unattended_continuations,Some(0)); assert_eq!(recorded.last_continuation_signature.as_deref(),Some("sig"));
        assert_eq!(crate::store::read_goal(&reference).unwrap(),Some(recorded));
    }
    #[tokio::test] async fn guard_denial_persists_block_but_single_flight_does_not() {
        let dir=tempfile::tempdir().unwrap(); let reference=crate::types::GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() };
        let goal=crate::store::create_goal(&reference,"work",None,0).await.unwrap();
        let mut input=admission(&goal); input.continuation_pending=true;
        let (same,_)=admit_and_record_goal_continuation(&reference,&input,1).await.unwrap(); assert_eq!(same,Some(goal.clone()));
        input.continuation_pending=false; input.consecutive_continuations=crate::continuation::GOAL_CONTINUATION_CAP;
        let (blocked,_)=admit_and_record_goal_continuation(&reference,&input,2).await.unwrap(); assert_eq!(blocked.unwrap().status,GoalStatus::Blocked);
    }
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
