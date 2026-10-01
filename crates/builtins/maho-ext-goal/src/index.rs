use crate::{types::{Goal,GoalStatus,GoalAccountingMode,GoalStoreRef},turn_usage::TurnUsageTracker,errors::GoalError};
use maho_agent::types::AgentMessage;
pub struct AgentGoalAccounting { pub goal_id:String,pub measured_from_milliseconds:f64 }
#[derive(Default)]
pub struct GoalTurnAccounting {
    pub turn_in_progress:bool,pub window:Option<AgentGoalAccounting>,
    pub blocked_this_turn:Option<String>,pub completed_this_turn:Option<String>,
    pub usage:TurnUsageTracker,
}
impl GoalTurnAccounting {
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
