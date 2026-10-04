use crate::{types::{Goal,GoalStatus,GoalAccountingMode,GoalStoreRef},turn_usage::TurnUsageTracker,errors::GoalError};
use maho_agent::types::AgentMessage;
pub fn count_trailing_goal_continuation_entries(entries:&[maho_ext_api::SessionEntry])->usize {
    let mut count=0;
    for entry in entries.iter().rev() {
        if entry.kind=="message"&&entry.data.get("message").and_then(|message|message.get("role")).and_then(serde_json::Value::as_str)==Some("user") { break; }
        if entry.kind=="custom_message"&&entry.data.get("customType").and_then(serde_json::Value::as_str)==Some("goal-continuation") { count+=1; }
    }
    count
}
pub struct AgentGoalAccounting { pub goal_id:String,pub measured_from_milliseconds:f64 }
#[derive(Default)]
pub struct GoalTurnAccounting {
    pub turn_in_progress:bool,pub window:Option<AgentGoalAccounting>,
    pub blocked_this_turn:Option<String>,pub completed_this_turn:Option<String>,
    pub usage:TurnUsageTracker,
}
impl GoalTurnAccounting {
    pub fn agent_start(&mut self,goal:Option<&Goal>,now:f64) {
        self.turn_in_progress=true; self.blocked_this_turn=None; self.completed_this_turn=None; self.usage.reset();
        if let Some(goal)=goal.filter(|goal|goal.status==GoalStatus::Active) { self.begin(goal,now); } else { self.window=None; }
    }
    pub async fn agent_end(&mut self,reference:&GoalStoreRef,messages:&[AgentMessage],user_aborted:bool,now:f64,epoch_seconds:u64)->Result<Option<Goal>,GoalError> {
        let mode=if self.blocked_this_turn.is_some() { GoalAccountingMode::ActiveOrBlocked } else if self.completed_this_turn.is_some() { GoalAccountingMode::ActiveOrComplete } else { GoalAccountingMode::Active };
        let mut goal=self.account(reference,mode,Some(messages),now,epoch_seconds).await?;
        self.turn_in_progress=false; self.blocked_this_turn=None; self.completed_this_turn=None;
        if user_aborted&&goal.as_ref().is_some_and(|goal|goal.status==GoalStatus::Active) {
            goal=Some(crate::store::update_goal(reference,&crate::types::GoalUpdate { status:Some(GoalStatus::Blocked),reason:Some("user interrupted the turn".into()),..Default::default() },crate::types::GoalUpdateSource::Model,epoch_seconds).await?);
        }
        if let Some(goal)=goal.as_ref().filter(|goal|goal.status==GoalStatus::Active) { self.begin(goal,now); } else { self.clear(); }
        Ok(goal)
    }
    pub fn begin(&mut self,goal:&Goal,now:f64) {
        if goal.status!=GoalStatus::Active || self.window.as_ref().is_some_and(|window|window.goal_id==goal.id) { return; }
        self.usage.discard_pending(); self.window=Some(AgentGoalAccounting { goal_id:goal.id.clone(),measured_from_milliseconds:now });
    }
    pub fn mark_blocked(&mut self,goal:&Goal) { if self.turn_in_progress { self.blocked_this_turn=Some(goal.id.clone()); } }
    pub fn mark_completed(&mut self,goal:&Goal,now:f64) {
        if !self.turn_in_progress { return; }
        self.completed_this_turn=Some(goal.id.clone()); self.window=Some(AgentGoalAccounting { goal_id:goal.id.clone(),measured_from_milliseconds:now });
    }
    pub fn stop(&mut self,id:&str) {
        if self.window.as_ref().is_some_and(|window|window.goal_id==id) { self.window=None; }
        if self.blocked_this_turn.as_deref()==Some(id) { self.blocked_this_turn=None; }
        if self.completed_this_turn.as_deref()==Some(id) { self.completed_this_turn=None; }
    }
    pub fn clear(&mut self) { self.window=None; self.blocked_this_turn=None; self.completed_this_turn=None; }
    pub async fn refresh_ui(&self,ticker:&mut crate::elapsed_ticker::GoalElapsedTicker,ctx:&maho_ext_api::ExtensionContext,goal:Option<&Goal>)->Result<(),maho_ext_api::ExtensionFailure> {
        if ctx.has_ui&&let Some(goal)=goal.filter(|goal|goal.status==GoalStatus::Active)&&let Some(window)=self.window.as_ref().filter(|window|window.goal_id==goal.id) {
            ticker.sync(ctx.clone(),goal.clone(),window.measured_from_milliseconds).await?;
            return Ok(());
        }
        if let Some(worker)=ticker.stop()? {
            match worker.await { Ok(result)=>result?,Err(error) if error.is_cancelled()=>(),Err(error)=>return Err(maho_ext_api::ExtensionFailure::new(error.to_string())) }
        }
        crate::ui::update_goal_ui(ctx,goal,None); Ok(())
    }
    pub async fn account(&mut self,reference:&GoalStoreRef,mode:GoalAccountingMode,messages:Option<&[AgentMessage]>,now:f64,epoch_seconds:u64)->Result<Option<Goal>,GoalError> {
        let Some(window)=&self.window else { return crate::store::read_goal(reference); };
        let usage=match messages { Some(messages)=>self.usage.take_remaining(messages),None=>self.usage.take_pending() };
        let elapsed=((now-window.measured_from_milliseconds)/1000.0+0.5).floor().max(0.0);
        let goal=crate::store::account_goal_usage(reference,&usage,elapsed,mode,Some(&window.goal_id),epoch_seconds).await?;
        if goal.as_ref().is_some_and(|goal|goal.id==window.goal_id) { if let Some(window)=&mut self.window { window.measured_from_milliseconds=now; } } else { self.clear(); }
        Ok(goal)
    }
}
#[cfg(test)] mod tests {
    use super::*;
    #[tokio::test] async fn footer_ticker_runs_only_for_active_matching_accounting_window() {
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() };
        let mut goal=crate::store::create_goal(&reference,"work",None,0).await.unwrap(); let mut accounting=GoalTurnAccounting::default(); accounting.begin(&goal,0.0);
        let mut ticker=crate::elapsed_ticker::GoalElapsedTicker::new(std::sync::Arc::new(|_,_,_|Ok(())),std::sync::Arc::new(||0.0));
        let ctx=crate::test_context::context(); accounting.refresh_ui(&mut ticker,&ctx,Some(&goal)).await.unwrap(); assert!(ticker.running());
        goal.status=GoalStatus::Paused; accounting.refresh_ui(&mut ticker,&ctx,Some(&goal)).await.unwrap(); assert!(!ticker.running());
        goal.status=GoalStatus::Active; goal.id="replacement".into(); accounting.refresh_ui(&mut ticker,&ctx,Some(&goal)).await.unwrap(); assert!(!ticker.running());
    }
    #[test] fn history_counter_resets_only_at_real_user_message() {
        fn entry(kind:&str,data:serde_json::Value)->maho_ext_api::SessionEntry { maho_ext_api::SessionEntry { id:String::new(),parent_id:None,timestamp:String::new(),kind:kind.into(),data } }
        let continuation=||entry("custom_message",serde_json::json!({"customType":"goal-continuation"}));
        let mut entries=vec![continuation(),entry("message",serde_json::json!({"message":{"role":"user"}})),continuation(),entry("message",serde_json::json!({"message":{"role":"assistant"}})),entry("custom_message",serde_json::json!({"customType":"notice"})),continuation()];
        assert_eq!(count_trailing_goal_continuation_entries(&entries),2);
        entries.push(entry("message",serde_json::json!({"message":{"role":"user"}}))); assert_eq!(count_trailing_goal_continuation_entries(&entries),0);
        assert_eq!(count_trailing_goal_continuation_entries(&[]),0);
    }
    #[tokio::test] async fn user_abort_accounts_then_blocks_and_retires_window() {
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() };
        let goal=crate::store::create_goal(&reference,"work",None,0).await.unwrap();
        let mut accounting=GoalTurnAccounting::default(); accounting.agent_start(Some(&goal),0.0);
        let ended=accounting.agent_end(&reference,&[],true,1500.0,1).await.unwrap().unwrap();
        assert_eq!(ended.status,GoalStatus::Blocked); assert_eq!(ended.time_used_seconds,2.0); assert_eq!(ended.blocked_reason.as_deref(),Some("user interrupted the turn"));
        assert!(!accounting.turn_in_progress); assert!(accounting.window.is_none());
    }
    #[tokio::test] async fn completed_goal_accounts_final_turn_without_reopening() {
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() };
        let goal=crate::store::create_goal(&reference,"work",None,0).await.unwrap();
        let mut accounting=GoalTurnAccounting::default(); accounting.agent_start(Some(&goal),0.0);
        let completed=crate::store::update_goal(&reference,&crate::types::GoalUpdate { status:Some(GoalStatus::Complete),..Default::default() },crate::types::GoalUpdateSource::Model,1).await.unwrap();
        accounting.mark_completed(&completed,1000.0);
        let ended=accounting.agent_end(&reference,&[],false,2500.0,2).await.unwrap().unwrap();
        assert_eq!(ended.status,GoalStatus::Complete); assert_eq!(ended.time_used_seconds,2.0); assert!(accounting.window.is_none());
    }
    #[tokio::test] async fn checkpoints_do_not_double_count_elapsed_and_replacement_retires_window() {
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() };
        let goal=crate::store::create_goal(&reference,"first",None,0).await.unwrap();
        let mut accounting=GoalTurnAccounting::default(); accounting.begin(&goal,0.0); accounting.begin(&goal,400.0);
        let first=accounting.account(&reference,GoalAccountingMode::Active,None,1500.0,1).await.unwrap().unwrap(); assert_eq!(first.time_used_seconds,2.0);
        let second=accounting.account(&reference,GoalAccountingMode::Active,None,2000.0,2).await.unwrap().unwrap(); assert_eq!(second.time_used_seconds,3.0);
        crate::store::clear_goal(&reference).await.unwrap();
        let replacement=crate::store::create_goal(&reference,"second",None,3).await.unwrap();
        let result=accounting.account(&reference,GoalAccountingMode::Active,None,5000.0,5).await.unwrap().unwrap();
        assert_eq!(result.id,replacement.id); assert_eq!(result.time_used_seconds,0.0); assert!(accounting.window.is_none());
    }
    #[tokio::test] async fn completion_starts_final_usage_window_only_in_running_turn() {
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() };
        let goal=crate::store::create_goal(&reference,"work",None,0).await.unwrap();
        let mut accounting=GoalTurnAccounting::default(); accounting.mark_completed(&goal,500.0); assert!(accounting.window.is_none());
        accounting.turn_in_progress=true; accounting.mark_completed(&goal,500.0); accounting.mark_blocked(&goal);
        assert_eq!(accounting.completed_this_turn.as_deref(),Some(goal.id.as_str()));
        accounting.stop("other"); assert!(accounting.window.is_some()); accounting.stop(&goal.id);
        assert!(accounting.window.is_none()); assert!(accounting.blocked_this_turn.is_none()); assert!(accounting.completed_this_turn.is_none());
    }
}
