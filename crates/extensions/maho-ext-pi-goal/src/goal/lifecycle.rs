use super::{types::*,turn_usage::TurnUsageTracker,store::{read_goal,account_goal_usage_at}};
use std::sync::{Arc,Mutex};
use maho_ext_api::{ExtensionApi,ExtensionContext,ExtensionEvent,ExtensionFailure,EventKind,EventResult,SessionReason};
#[derive(Default)]
pub struct GoalLifecycle{
    pub agent_turn_in_progress:bool,
    pub agent_goal_accounting:Option<(String,u64)>,
    pub blocked_this_turn_goal_id:Option<String>,
    pub completed_this_turn_goal_id:Option<String>,
    pub turn_usage:TurnUsageTracker,
}
pub type GoalStoreResolver=Arc<dyn Fn(&ExtensionContext)->GoalStoreRef+Send+Sync>;
pub type GoalContinuationSender=Arc<dyn Fn(String)->Result<(),ExtensionFailure>+Send+Sync>;
pub struct RegisteredGoalLifecycle{pub state:Arc<Mutex<GoalLifecycle>>,pub resolve:GoalStoreResolver,pub send:GoalContinuationSender}
impl super::command_registration::GoalCommandRegistrationDeps for RegisteredGoalLifecycle{
    fn goal_store_ref(&self,ctx:&ExtensionContext)->GoalStoreRef{(self.resolve)(ctx)}
    fn begin_agent_goal_accounting(&self,goal:&Goal){self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).begin_agent_goal_accounting(goal,now_milliseconds());}
    fn stop_agent_goal_accounting(&self,id:&str){self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).stop_agent_goal_accounting(id);}
    fn clear_agent_goal_accounting(&self){self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear_agent_goal_accounting();}
    fn account_current_agent_turn<'a>(&'a self,ctx:&'a ExtensionContext,mode:GoalAccountingMode)->maho_ext_api::ExtensionFuture<'a,Option<Goal>>{Box::pin(async move{self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).account_current_agent_turn(&(self.resolve)(ctx),mode,None,now_milliseconds()).map_err(ExtensionFailure::new)})}
    fn queue_goal_continuation(&self,ctx:&ExtensionContext,goal:&Goal)->Result<(),ExtensionFailure>{queue_goal_continuation(ctx,goal,self.send.as_ref())}
}
pub fn now_milliseconds()->u64{u64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis()).unwrap_or(u64::MAX)}
pub fn register_goal_lifecycle(api:&mut ExtensionApi,resolve:GoalStoreResolver,send:GoalContinuationSender)->Arc<Mutex<GoalLifecycle>>{
    let state=Arc::new(Mutex::new(GoalLifecycle::default()));
    let abort=Arc::new(Mutex::new(None::<maho_ext_api::AbortSignal>));
    for kind in [EventKind::SessionStart,EventKind::BeforeAgentStart,EventKind::AgentStart,EventKind::MessageEnd,EventKind::AgentEnd,EventKind::SessionShutdown]{
        let state=Arc::clone(&state);let resolve=Arc::clone(&resolve);let send=Arc::clone(&send);let abort=Arc::clone(&abort);
        api.on(kind,Arc::new(move|event,ctx|{
            let state=Arc::clone(&state);let resolve=Arc::clone(&resolve);let send=Arc::clone(&send);let abort=Arc::clone(&abort);
            Box::pin(async move{
                let reference=resolve(ctx);
                match event{
                    ExtensionEvent::SessionStart(start)=>{
                        let goal=read_goal(&reference).map_err(ExtensionFailure::new)?;
                        {let mut state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if let Some(goal)=goal.as_ref().filter(|goal|goal.status==GoalStatus::Active){state.begin_agent_goal_accounting(goal,now_milliseconds());}else{state.clear_agent_goal_accounting();}}
                        super::ui::update_goal_ui(ctx,goal.as_ref());
                        if start.reason==SessionReason::Resume&&let Some(goal)=goal.as_ref().filter(|goal|goal.status==GoalStatus::Paused)&&ctx.has_ui&&(ctx.is_idle_fn)()&&!ctx.has_pending_messages()?{
                            let choice=ctx.ui.select(&format!("Resume paused goal?\nGoal: {}",goal.objective),&["Resume goal".into(),"Leave paused".into()],Default::default()).await;
                            if choice.as_deref()==Some("Resume goal"){
                                let resumed=super::store::update_goal(&reference,&GoalUpdate{status:Some(GoalStatus::Active),..Default::default()},GoalUpdateSource::User).map_err(ExtensionFailure::new)?;
                                state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).begin_agent_goal_accounting(&resumed,now_milliseconds());
                                super::ui::update_goal_ui(ctx,Some(&resumed));
                                ctx.ui.notify(&format!("Goal {}\n{}",super::format::goal_status_label(resumed.status),super::format::format_goal_for_tool(Some(&resumed)).map_err(ExtensionFailure::new)?),maho_ext_api::NotificationType::Info);
                                queue_goal_continuation(ctx,&resumed,send.as_ref())?;
                            }
                            return Ok(EventResult::None);
                        }
                        if let Some(goal)=goal{queue_goal_continuation(ctx,&goal,send.as_ref())?;}
                    }
                    ExtensionEvent::BeforeAgentStart(_) if read_goal(&reference).map_err(ExtensionFailure::new)?.is_some_and(|goal|goal.status==GoalStatus::Blocked)=>{
                        let resumed=super::store::update_goal(&reference,&GoalUpdate{status:Some(GoalStatus::Active),..Default::default()},GoalUpdateSource::User).map_err(ExtensionFailure::new)?;super::ui::update_goal_ui(ctx,Some(&resumed));
                    }
                    ExtensionEvent::AgentStart=>{
                        *abort.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=ctx.signal.clone();
                        let goal=read_goal(&reference).map_err(ExtensionFailure::new)?;
                        let mut state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);state.agent_turn_in_progress=true;state.turn_usage.reset();state.blocked_this_turn_goal_id=None;state.completed_this_turn_goal_id=None;
                        if let Some(goal)=goal.as_ref().filter(|goal|goal.status==GoalStatus::Active){state.begin_agent_goal_accounting(goal,now_milliseconds());}else{state.agent_goal_accounting=None;}
                    }
                    ExtensionEvent::MessageEnd{message}=>{let message=serde_json::to_value(message).map_err(|error|ExtensionFailure::new(error.to_string()))?;state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).turn_usage.note_message_end(&message);}
                    ExtensionEvent::AgentEnd{messages,..}=>{
                        let aborted=abort.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take().is_some_and(|signal|signal.is_aborted());
                        let messages=messages.iter().map(serde_json::to_value).collect::<Result<Vec<_>,_>>().map_err(|error|ExtensionFailure::new(error.to_string()))?;
                        let mut goal={let mut state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);let mode=if state.blocked_this_turn_goal_id.is_some(){GoalAccountingMode::ActiveOrBlocked}else if state.completed_this_turn_goal_id.is_none(){GoalAccountingMode::Active}else{GoalAccountingMode::ActiveOrComplete};let goal=state.account_current_agent_turn(&reference,mode,Some(&messages),now_milliseconds()).map_err(ExtensionFailure::new)?;state.agent_turn_in_progress=false;state.blocked_this_turn_goal_id=None;state.completed_this_turn_goal_id=None;goal};
                        if aborted&&goal.as_ref().is_some_and(|goal|goal.status==GoalStatus::Active){goal=Some(super::store::update_goal(&reference,&GoalUpdate{status:Some(GoalStatus::Blocked),reason:Some("user interrupted the turn".into()),..Default::default()},GoalUpdateSource::Model).map_err(ExtensionFailure::new)?);}
                        {let mut state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if let Some(goal)=goal.as_ref().filter(|goal|goal.status==GoalStatus::Active){state.begin_agent_goal_accounting(goal,now_milliseconds());}else{state.clear_agent_goal_accounting();}}
                        super::ui::update_goal_ui(ctx,goal.as_ref());
                        if super::continuation::should_queue_goal_continuation_after_agent_end(goal.as_ref(),ctx.has_pending_messages()?)&&let Some(goal)=goal{send(super::prompt::build_continuation_prompt(&goal))?;}
                    }
                    ExtensionEvent::SessionShutdown(_)=>{let mut state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if state.agent_goal_accounting.is_some(){state.account_current_agent_turn(&reference,GoalAccountingMode::Active,None,now_milliseconds()).map_err(ExtensionFailure::new)?;}state.clear_agent_goal_accounting();}
                    _=>{}
                }
                Ok(EventResult::None)
            })
        }));
    }
    state
}
pub fn queue_goal_continuation(ctx:&ExtensionContext,goal:&Goal,send:&(dyn Fn(String)->Result<(),ExtensionFailure>+Send+Sync))->Result<(),ExtensionFailure>{
    if super::continuation::should_queue_goal_continuation_when_idle(Some(goal),(ctx.is_idle_fn)(),ctx.has_pending_messages()?){send(super::prompt::build_continuation_prompt(goal))?;}Ok(())
}
impl GoalLifecycle{
    pub fn begin_agent_goal_accounting(&mut self,goal:&Goal,now_ms:u64){if goal.status!=GoalStatus::Active||self.agent_goal_accounting.as_ref().is_some_and(|(id,_)|id==&goal.id){return;}self.turn_usage.discard_pending();self.agent_goal_accounting=Some((goal.id.clone(),now_ms));}
    pub fn mark_goal_blocked_this_turn(&mut self,goal:&Goal){if self.agent_turn_in_progress{self.blocked_this_turn_goal_id=Some(goal.id.clone());}}
    pub fn mark_goal_completed_this_turn(&mut self,goal:&Goal,now_ms:u64){if !self.agent_turn_in_progress{return;}self.completed_this_turn_goal_id=Some(goal.id.clone());self.agent_goal_accounting=Some((goal.id.clone(),now_ms));}
    pub fn stop_agent_goal_accounting(&mut self,id:&str){
        if self.agent_goal_accounting.as_ref().is_some_and(|(current,_)|current==id){self.agent_goal_accounting=None;}
        if self.blocked_this_turn_goal_id.as_deref()==Some(id){self.blocked_this_turn_goal_id=None;}
        if self.completed_this_turn_goal_id.as_deref()==Some(id){self.completed_this_turn_goal_id=None;}
    }
    pub fn clear_agent_goal_accounting(&mut self){self.agent_goal_accounting=None;self.blocked_this_turn_goal_id=None;self.completed_this_turn_goal_id=None;}
    pub fn account_current_agent_turn(&mut self,reference:&GoalStoreRef,mode:GoalAccountingMode,messages:Option<&[serde_json::Value]>,now_ms:u64)->Result<Option<Goal>,String>{
        let Some((id,measured))=self.agent_goal_accounting.clone()else{return read_goal(reference);};
        let usage=match messages{Some(messages)=>self.turn_usage.take_remaining(messages),None=>self.turn_usage.take_pending()};
        let elapsed=now_ms.saturating_sub(measured).saturating_add(500)/1000;
        let goal=account_goal_usage_at(reference,&usage,elapsed.to_string().parse().unwrap_or_else(|_|unreachable!("u64 fits f64")),mode,Some(&id),now_ms/1000)?;
        if goal.as_ref().is_some_and(|goal|goal.id==id){self.agent_goal_accounting=Some((id,now_ms));}else{self.clear_agent_goal_accounting();}Ok(goal)
    }
}
