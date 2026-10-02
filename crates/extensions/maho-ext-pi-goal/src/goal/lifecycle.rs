use super::{types::*,turn_usage::TurnUsageTracker,store::{read_goal,account_goal_usage_at}};
#[derive(Default)]
pub struct GoalLifecycle{
    pub agent_turn_in_progress:bool,
    pub agent_goal_accounting:Option<(String,u64)>,
    pub blocked_this_turn_goal_id:Option<String>,
    pub completed_this_turn_goal_id:Option<String>,
    pub turn_usage:TurnUsageTracker,
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
