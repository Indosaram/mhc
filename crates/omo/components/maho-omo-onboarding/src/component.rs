use std::{path::PathBuf, sync::{Arc, Mutex}};
use maho_ext_api::*;
use crate::state::{claim_onboarding, is_onboarding_complete, release_onboarding};

pub struct OnboardingComponent { pub state_dir: PathBuf, pub skills_root: String }

#[derive(Default)]
struct PendingRun { session_id: String, owned_marker: bool, active: bool, end: Option<(bool, Option<AbortSource>)> }

fn session_id(ctx: &ExtensionContext) -> String {
    ctx.session_manager.get_leaf_id().unwrap_or_default()
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
        let pending=Arc::new(Mutex::new(None::<PendingRun>));

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
                *pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=Some(PendingRun {session_id:session_id(ctx),owned_marker:owned,active:false,end:None});
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
                let ExtensionEvent::BeforeAgentStart(_event)=event else {return Ok(EventResult::None);};
                if !eligible(ctx,session_kind,&runtime) {return Ok(EventResult::None);}
                let forced=runtime.get_flag("onboard")==Some(FlagValue::Boolean(true))
                    && !*process_forced.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                {
                    let mut current=pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if current.is_some() {return Ok(EventResult::None);}
                    let owned=claim_onboarding(&state_dir);
                    if !owned && !forced {return Ok(EventResult::None);}
                    if forced { *process_forced.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=true; }
                    *current=Some(PendingRun {session_id:session_id(ctx),owned_marker:owned,active:false,end:None});
                }
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
                    let Some(run)=current.as_mut() else {return Ok(EventResult::None);};
                    if run.session_id!=session_id(ctx) {return Ok(EventResult::None);}
                    let mut release=false;
                    let mut settled_success=false;
                    match (&kind,event) {
                        (EventKind::AgentStart,ExtensionEvent::AgentStart)=>{run.active=true;run.end=None;}
                        (EventKind::AgentEnd,ExtensionEvent::AgentEnd{aborted,will_retry,abort_source,..}) if run.active=>{
                            run.end=Some((will_retry.unwrap_or(false),*abort_source));
                            release=!will_retry.unwrap_or(false) && (aborted.unwrap_or(false) || abort_source.is_some());
                        }
                        (EventKind::AgentSettled,ExtensionEvent::AgentSettled) if run.active=>{
                            release=run.end.is_some_and(|(retry,source)| !retry && source.is_some());
                            settled_success = !release;
                        }
                        (EventKind::InputDisposition,ExtensionEvent::InputDisposition{disposition,..}) if !run.active=>release=*disposition==InputDisposition::Rejected,
                        (EventKind::SessionAbort,ExtensionEvent::SessionAbort)|(EventKind::SessionShutdown,ExtensionEvent::SessionShutdown(_))=>release=true,
                        _=>{}
                    }
                    let owned=run.owned_marker;
                    if release || settled_success {
                        *current=None;
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
