use std::{path::PathBuf, sync::{Arc, Mutex}};
use maho_ext_api::*;
use maho_omo_start_work_continuation::agent_end_eligibility::read_agent_end_outcome;
use crate::state::{claim_onboarding, is_onboarding_complete, release_onboarding};

pub struct OnboardingComponent { pub state_dir: PathBuf, pub skills_root: String }

#[derive(Default)]
struct PendingRun { session_id: String, owned_marker: bool, active: bool, end: Option<JsonValue> }

fn session_id(ctx: &ExtensionContext) -> String {
    ctx.session_manager.session_id().to_owned()
}

#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod test_support;

#[cfg(test)]
mod tests {
    use super::*;

    fn register(path: &std::path::Path) -> ExtensionApi {
        let mut api=ExtensionApi::new(LoadedExtension::new("onboarding","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
        OnboardingComponent {state_dir:path.into(),skills_root:"/skills".into()}.register(&mut api);
        api
    }

    fn prompt(trigger: &str, preview: bool) -> ExtensionEvent {
        ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent {prompt:"hello".into(),images:None,system_prompt:"base".into(),system_prompt_options:Default::default(),trigger:trigger.into(),preview})
    }

    #[test]
    fn ownership_uses_session_identity_without_a_leaf() {
        let ctx=test_support::context();
        assert_eq!(ctx.session_manager.get_leaf_id(),None);
        assert_eq!(session_id(&ctx),"session");
    }

    #[tokio::test]
    async fn pending_claim_is_released_by_abort_shutdown_and_rejection() {
        for mut event in [ExtensionEvent::SessionAbort,ExtensionEvent::SessionShutdown(SessionShutdownEvent {reason:SessionReason::Quit,target_session_file:None,signal:None}),ExtensionEvent::InputDisposition {input_id:"input".into(),disposition:InputDisposition::Rejected}] {
            let root=tempfile::tempdir().expect("isolated onboarding directory");
            let api=register(root.path());
            let ctx=test_support::context();
            dispatch(&api,&mut prompt("prompt",false),&ctx).await;
            assert!(is_onboarding_complete(root.path()));
            dispatch(&api,&mut event,&ctx).await;
            assert!(!is_onboarding_complete(root.path()));
            assert!(matches!(dispatch(&api,&mut prompt("prompt",false),&ctx).await,EventResult::BeforeAgentStart(_)));
        }
    }

    async fn dispatch(api: &ExtensionApi, event: &mut ExtensionEvent, ctx: &ExtensionContext) -> EventResult {
        api.registered.handlers[&event.kind()][0](event,ctx).await.expect("onboarding event dispatch succeeds")
    }

    fn end(payload: JsonValue) -> ExtensionEvent {
        let messages=payload["messages"].as_array().expect("message array fixture").iter().map(|message| {
            let mut complete=serde_json::json!({"role":"assistant","content":[],"api":"test","provider":"test","model":"test","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"timestamp":0,"toolCallId":"call-1","toolName":"bash"});
            complete.as_object_mut().expect("message object").extend(message.as_object().expect("fixture object").clone());
            serde_json::from_value(complete).expect("valid agent message fixture")
        }).collect();
        ExtensionEvent::AgentEnd {messages,aborted:payload["aborted"].as_bool(),will_retry:payload["willRetry"].as_bool(),abort_source:None}
    }

    #[tokio::test]
    async fn previews_and_non_prompt_turns_do_not_claim() {
        for (trigger,preview) in [("extension",false),("delivery",false),("prompt",true)] {
            let root=tempfile::tempdir().expect("isolated onboarding directory");
            let api=register(root.path());
            let ctx=test_support::context();
            assert!(matches!(dispatch(&api,&mut prompt(trigger,preview),&ctx).await,EventResult::None));
            assert!(!is_onboarding_complete(root.path()));
            assert!(matches!(dispatch(&api,&mut prompt("prompt",false),&ctx).await,EventResult::BeforeAgentStart(_)));
        }
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
            let root=tempfile::tempdir().expect("isolated onboarding directory");
            let api=register(root.path());
            let ctx=test_support::context();
            dispatch(&api,&mut prompt("prompt",false),&ctx).await;
            dispatch(&api,&mut ExtensionEvent::AgentStart,&ctx).await;
            let outcome=read_agent_end_outcome(&payload);
            assert!(outcome.blocked_by.is_some());
            dispatch(&api,&mut end(payload),&ctx).await;
            assert_eq!(is_onboarding_complete(root.path()),outcome.will_retry);
            dispatch(&api,&mut ExtensionEvent::AgentSettled,&ctx).await;
            assert!(!is_onboarding_complete(root.path()));
        }
    }

    #[tokio::test]
    async fn completion_blocks_reclaim_even_if_marker_is_removed() {
        let root=tempfile::tempdir().expect("isolated onboarding directory");
        let api=register(root.path());
        let ctx=test_support::context();
        dispatch(&api,&mut prompt("prompt",false),&ctx).await;
        dispatch(&api,&mut ExtensionEvent::AgentStart,&ctx).await;
        dispatch(&api,&mut end(serde_json::json!({"messages":[{"role":"assistant","stopReason":"stop"}]})),&ctx).await;
        dispatch(&api,&mut ExtensionEvent::AgentSettled,&ctx).await;
        release_onboarding(root.path()).expect("remove completed marker");
        assert!(matches!(dispatch(&api,&mut prompt("prompt",false),&ctx).await,EventResult::None));
        assert!(!is_onboarding_complete(root.path()));
    }
}

fn eligible(ctx: &ExtensionContext, session_kind: SessionKind, runtime: &ExtensionRuntime) -> bool {
    ctx.has_ui && session_kind == SessionKind::Interactive
        && runtime.get_flag("omo-senpi-onboarding-disabled") != Some(FlagValue::Boolean(true))
}

fn show_welcome(ctx: &ExtensionContext) {
    ctx.ui.set_widget("omo-onboarding", Some(WidgetContent::Lines(vec![
        "Welcome to OmO. Ask anything.".into(),
        "Use ulw for full workflows.".into(),
    ])), ExtensionWidgetOptions::default());
}

impl Extension for OnboardingComponent {
    fn register(&self, api: &mut ExtensionApi) {
        api.register_flag("onboard",FlagType::Boolean{default:Some(false)},Some("Start the guided tour.".into()));
        let runtime=api.runtime.clone();
        let session_kind=api.profile.session_kind;
        let state_dir=self.state_dir.clone();
        let skills_root=self.skills_root.clone();
        let process_forced=Arc::new(Mutex::new(false));
        // None is unclaimed; Some(None) is a successfully completed turn.
        let pending=Arc::new(Mutex::new(None::<Option<PendingRun>>));

        let startup_runtime=runtime.clone();
        let startup_dir=state_dir.clone();
        let startup_skills=skills_root.clone();
        let startup_forced=Arc::clone(&process_forced);
        let startup_pending=Arc::clone(&pending);
        api.on(EventKind::SessionStart,Arc::new(move |event,ctx| {
            let runtime=startup_runtime.clone();let state_dir=startup_dir.clone();let skills_root=startup_skills.clone();
            let process_forced=Arc::clone(&startup_forced);let pending=Arc::clone(&startup_pending);
            Box::pin(async move {
                let ExtensionEvent::SessionStart(event)=event else { return Ok(EventResult::None); };
                if event.reason!=SessionReason::Startup || ctx.mode!=ExtensionMode::Tui || !eligible(ctx,session_kind,&runtime) {return Ok(EventResult::None);}
                let forced=runtime.get_flag("onboard")==Some(FlagValue::Boolean(true))
                    && !*process_forced.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if !forced {
                    if !is_onboarding_complete(&state_dir) { show_welcome(ctx); }
                    return Ok(EventResult::None);
                }
                if pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_some() {return Ok(EventResult::None);}
                let owned=claim_onboarding(&state_dir);
                *process_forced.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=true;
                *pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=Some(Some(PendingRun {session_id:session_id(ctx),owned_marker:owned,active:false,end:None}));
                ctx.ui.set_widget("omo-onboarding",None,ExtensionWidgetOptions::default());
                let message=CustomMessage {custom_type:"omo-onboarding:bootstrap".into(),content:vec![ToolContent::text(format!("Follow {skills_root}/onboarding/SKILL.md. Greet first."))],display:false,details:None};
                let actions=runtime.message_actions()?;
                if let Err(error)=actions.send_message(message,SendMessageOptions{trigger_turn:true,deliver_as:Some(DeliverAs::FollowUp)}) {
                    if owned {let _=release_onboarding(&state_dir);}
                    *pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=None;
                    if !is_onboarding_complete(&state_dir) {show_welcome(ctx);}
                    return Err(error);
                }
                Ok(EventResult::None)
            })
        }));

        let prompt_runtime=runtime.clone();let prompt_dir=state_dir.clone();let prompt_skills=skills_root.clone();
        let prompt_forced=Arc::clone(&process_forced);let prompt_pending=Arc::clone(&pending);
        api.on(EventKind::BeforeAgentStart,Arc::new(move |event,ctx| {
            let runtime=prompt_runtime.clone();let state_dir=prompt_dir.clone();let skills_root=prompt_skills.clone();
            let process_forced=Arc::clone(&prompt_forced);let pending=Arc::clone(&prompt_pending);
            Box::pin(async move {
                let ExtensionEvent::BeforeAgentStart(event)=event else {return Ok(EventResult::None);};
                if event.trigger!="prompt" || event.preview || !eligible(ctx,session_kind,&runtime) {return Ok(EventResult::None);}
                let forced=runtime.get_flag("onboard")==Some(FlagValue::Boolean(true))
                    && !*process_forced.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                {
                    let mut current=pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if current.is_some() {return Ok(EventResult::None);}
                    let owned=claim_onboarding(&state_dir);
                    if !owned && !forced {return Ok(EventResult::None);}
                    if forced { *process_forced.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=true; }
                    *current=Some(Some(PendingRun {session_id:session_id(ctx),owned_marker:owned,active:false,end:None}));
                }
                if ctx.mode==ExtensionMode::Tui {ctx.ui.set_widget("omo-onboarding",None,ExtensionWidgetOptions::default());}
                if ctx.session_manager.get_branch().iter().any(|entry| entry.kind=="custom_message" && entry.data.get("customType").and_then(JsonValue::as_str)==Some("omo-onboarding:context")) {
                    return Ok(EventResult::None);
                }
                let content=if forced {format!("Follow {skills_root}/onboarding/SKILL.md. Tour using their message.")}
                    else {format!("Answer fully; greet in their language; offer a one-line tour. On request/acceptance: Follow {skills_root}/onboarding/SKILL.md.")};
                Ok(EventResult::BeforeAgentStart(BeforeAgentStartEventResult {message:Some(CustomMessage {custom_type:"omo-onboarding:context".into(),content:vec![ToolContent::text(content)],display:false,details:None}),system_prompt:None}))
            })
        }));

        for kind in [EventKind::AgentStart,EventKind::AgentEnd,EventKind::AgentSettled,EventKind::InputDisposition,EventKind::SessionAbort,EventKind::SessionShutdown] {
            let runtime=runtime.clone();let state_dir=state_dir.clone();let pending=Arc::clone(&pending);let session_kind=api.profile.session_kind;
            api.on(kind,Arc::new(move |event,ctx| {
                let runtime=runtime.clone();let state_dir=state_dir.clone();let pending=Arc::clone(&pending);let session_kind=session_kind;
                Box::pin(async move {
                    let mut current=pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
                    if release {
                        if owned {release_onboarding(&state_dir).map_err(|error|ExtensionFailure::new(error.to_string()))?;}
                        if eligible(ctx,session_kind,&runtime) && ctx.mode==ExtensionMode::Tui && !is_onboarding_complete(&state_dir) {show_welcome(ctx);}
                    }
                    Ok(EventResult::None)
                })
            }));
        }
    }
}
