use std::collections::{BTreeMap,BTreeSet};
use crate::wait_progress::GoalWaitKind;
pub const GOAL_CONTINUATION_SCHEDULED_EVENT:&str="goal_continuation_scheduled";
pub const GOAL_CONTINUATION_RESUMED_EVENT:&str="goal_continuation_resumed";
pub const GOAL_CONTINUATION_TIMER_STATE_EVENT:&str="goal_continuation_timer_state";
pub const GOAL_MONITOR_STALL_EVENT:&str="goal_monitor_continuation_stall";
#[derive(Clone,Copy,Debug,PartialEq)]
pub struct HeldTimer { pub kind:GoalWaitKind,pub remaining_ms:f64,pub held_at_ms:f64,pub total_ms:f64,pub drain_fire:bool }
#[derive(Clone,Copy,Debug,PartialEq)]
pub struct ArmedTimer { pub kind:GoalWaitKind,pub due_at_ms:f64,pub total_ms:f64,pub drain_fire:bool }
#[derive(Default)]
pub struct MonitorAwareGoalContinuation {
    goal:Option<crate::types::Goal>,
    pub wake_sources:BTreeMap<String,f64>,
    pub armed_timer:Option<ArmedTimer>,
    pub held_timer:Option<HeldTimer>,
    direct_input_holds:BTreeSet<String>,
    pub ended_turn_was_user_initiated:bool,
    pub ask_user_deadline_at_ms:Option<f64>,
    pub recent_normalized_output_hashes:Vec<String>,
    pub toolless_continuation_streak:u64,
    toolless_streak_goal_id:Option<String>,
    pub consecutive_length_recoveries:BTreeMap<String,u64>,
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum WakeSourceChange { Drained,QuestionDeadlineChanged,CountsChanged }
impl MonitorAwareGoalContinuation {
    pub fn sync_goal(&mut self,goal:Option<&crate::types::Goal>) {
        if goal.map(|goal|goal.id.as_str())!=self.goal.as_ref().map(|goal|goal.id.as_str()) { self.reset_continuation_state(); }
        self.goal=goal.cloned();
        if !goal.is_some_and(|goal|goal.status==crate::types::GoalStatus::Active) { self.armed_timer=None; self.held_timer=None; self.reset_continuation_state(); }
    }
    pub fn has_active_wake_sources(&self)->bool { self.wake_sources.values().sum::<f64>()>0.0 }
    pub fn hold_direct_input(&mut self,input_id:&str,now:f64) {
        if !self.direct_input_holds.insert(input_id.into()) || self.direct_input_holds.len()!=1 { return; }
        if let Some(timer)=self.armed_timer.take() {
            self.held_timer=Some(HeldTimer { kind:timer.kind,remaining_ms:(timer.due_at_ms-now).max(0.0),held_at_ms:now,total_ms:timer.total_ms,drain_fire:timer.drain_fire });
        }
    }
    pub fn resolve_direct_input(&mut self,input_id:&str,accepted:bool,now:f64) {
        if !self.direct_input_holds.remove(input_id) { return; }
        if accepted { self.held_timer=None; self.note_user_prompt(); return; }
        if !self.direct_input_holds.is_empty() { return; }
        if let Some(held)=self.held_timer.take() {
            let remaining=(held.remaining_ms-(now-held.held_at_ms).max(0.0)).max(0.0);
            self.arm_timer(held.kind,remaining,held.total_ms,held.drain_fire,now);
        }
    }
    pub fn arm_timer(&mut self,kind:GoalWaitKind,remaining_ms:f64,total_ms:f64,drain_fire:bool,now:f64) {
        if !self.direct_input_holds.is_empty() {
            self.held_timer=Some(HeldTimer { kind,remaining_ms,held_at_ms:now,total_ms,drain_fire });
            self.armed_timer=None;
        } else { self.armed_timer=Some(ArmedTimer { kind,due_at_ms:now+remaining_ms,total_ms,drain_fire }); }
    }
    pub fn note_user_prompt(&mut self) { self.armed_timer=None; self.held_timer=None; self.ended_turn_was_user_initiated=true; self.reset_continuation_state(); }
    pub fn note_continuation_started(&mut self) { self.ended_turn_was_user_initiated=false; }
    pub fn record_assistant_output(&mut self,text:&str,turn_used_tools:bool) {
        if turn_used_tools { self.recent_normalized_output_hashes.clear(); return; }
        if crate::continuation::normalize_assistant_text(text).is_empty() { return; }
        self.recent_normalized_output_hashes.push(crate::continuation::hash_assistant_text(text));
        if self.recent_normalized_output_hashes.len()>3 { self.recent_normalized_output_hashes.remove(0); }
    }
    pub fn record_toolless_continuation_turn(&mut self,goal_id:&str,turn_used_tools:bool) {
        if self.toolless_streak_goal_id.as_deref()!=Some(goal_id) { self.toolless_streak_goal_id=Some(goal_id.into()); self.toolless_continuation_streak=0; }
        if self.ended_turn_was_user_initiated { return; }
        if turn_used_tools { self.toolless_continuation_streak=0; } else { self.toolless_continuation_streak+=1; }
    }
    pub fn reset_continuation_state(&mut self) { self.consecutive_length_recoveries.clear(); self.recent_normalized_output_hashes.clear(); self.toolless_continuation_streak=0; self.toolless_streak_goal_id=None; }
    pub fn ask_user_wait_ms(&self,now:f64,idle_timeout_ms:f64)->Option<f64> {
        if self.wake_sources.get("ask-user").copied().unwrap_or(0.0)<=0.0 { return None; }
        let remaining=self.ask_user_deadline_at_ms.unwrap_or(0.0)-now;
        Some(crate::cache_warm::resolve_goal_monitor_continuation_delay_ms(Some(if remaining>0.0 { remaining/1000.0 } else { idle_timeout_ms/1000.0 })))
    }
    pub fn note_ask_user_wait(&mut self,count:f64,deadlines:&[f64],now:f64,idle_timeout_ms:f64) {
        if count<=0.0 { self.ask_user_deadline_at_ms=None; return; }
        if let Some(deadline)=deadlines.iter().copied().reduce(f64::min) { self.ask_user_deadline_at_ms=Some(deadline); return; }
        if count>self.wake_sources.get("ask-user").copied().unwrap_or(0.0) || self.ask_user_deadline_at_ms.is_none() { self.ask_user_deadline_at_ms=Some(now+idle_timeout_ms); }
    }
    pub fn set_wake_source_count(&mut self,source:&str,count:f64,deadlines:&[f64],now:f64,idle_timeout_ms:f64)->WakeSourceChange {
        let previous=self.wake_sources.values().sum::<f64>(); let deadline=self.ask_user_deadline_at_ms;
        if source=="ask-user" { self.note_ask_user_wait(count,deadlines,now,idle_timeout_ms); }
        self.wake_sources.insert(source.into(),count);
        if previous>0.0 && self.wake_sources.values().sum::<f64>()==0.0 {
            let kind=self.armed_timer.map(|timer|timer.kind).or_else(||self.held_timer.map(|timer|timer.kind));
            if kind==Some(GoalWaitKind::Monitor) { self.arm_timer(GoalWaitKind::Monitor,1000.0,1000.0,true,now); }
            self.toolless_continuation_streak=0; self.toolless_streak_goal_id=None;
            return WakeSourceChange::Drained;
        }
        if deadline!=self.ask_user_deadline_at_ms { WakeSourceChange::QuestionDeadlineChanged } else { WakeSourceChange::CountsChanged }
    }
    pub fn dispose(&mut self) { *self=Self::default(); }
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn stopped_goal_cancels_wait_and_changed_identity_resets_repetition() {
        let mut goal:crate::types::Goal=serde_json::from_value(serde_json::json!({"id":"g","threadId":"s","objective":"work","status":"active","tokensUsed":0,"timeUsedSeconds":0,"createdAt":0,"updatedAt":0})).unwrap();
        let mut monitor=MonitorAwareGoalContinuation::default(); monitor.sync_goal(Some(&goal)); monitor.record_assistant_output("same",false);
        monitor.sync_goal(Some(&goal)); assert_eq!(monitor.recent_normalized_output_hashes.len(),1);
        goal.id="next".into(); monitor.sync_goal(Some(&goal)); assert!(monitor.recent_normalized_output_hashes.is_empty());
        monitor.arm_timer(GoalWaitKind::Monitor,1000.0,1000.0,false,0.0); goal.status=crate::types::GoalStatus::Blocked; monitor.sync_goal(Some(&goal)); assert!(monitor.armed_timer.is_none());
    }
    #[test] fn overlapping_rejected_inputs_preserve_original_deadline() {
        let mut monitor=MonitorAwareGoalContinuation::default(); monitor.arm_timer(GoalWaitKind::Monitor,1000.0,1000.0,false,0.0);
        monitor.hold_direct_input("a",100.0); monitor.hold_direct_input("b",200.0);
        monitor.resolve_direct_input("a",false,300.0); assert!(monitor.armed_timer.is_none());
        monitor.resolve_direct_input("b",false,400.0); assert_eq!(monitor.armed_timer.unwrap().due_at_ms,1000.0);
    }
    #[test] fn accepted_input_cancels_held_wait() {
        let mut monitor=MonitorAwareGoalContinuation::default(); monitor.arm_timer(GoalWaitKind::Monitor,1000.0,1000.0,false,0.0);
        monitor.hold_direct_input("a",100.0); monitor.resolve_direct_input("a",true,200.0);
        assert!(monitor.held_timer.is_none() && monitor.armed_timer.is_none()); assert!(monitor.ended_turn_was_user_initiated);
    }
    #[test] fn earliest_question_deadline_overrides_count_heuristic() {
        let mut monitor=MonitorAwareGoalContinuation::default(); monitor.note_ask_user_wait(2.0,&[9000.0,5000.0],1000.0,30_000.0); monitor.wake_sources.insert("ask-user".into(),2.0);
        assert_eq!(monitor.ask_user_wait_ms(2000.0,30_000.0),Some(3000.0));
    }
    #[test] fn expired_question_deadline_uses_another_idle_window() {
        let mut monitor=MonitorAwareGoalContinuation::default(); monitor.note_ask_user_wait(1.0,&[1000.0],0.0,30_000.0); monitor.wake_sources.insert("ask-user".into(),1.0);
        assert_eq!(monitor.ask_user_wait_ms(2000.0,30_000.0),Some(30_000.0));
    }
    #[test] fn final_channel_drain_arms_one_second_resume() {
        let mut monitor=MonitorAwareGoalContinuation::default(); monitor.set_wake_source_count("task",1.0,&[],0.0,30_000.0); monitor.arm_timer(GoalWaitKind::Monitor,30_000.0,30_000.0,false,0.0);
        assert_eq!(monitor.set_wake_source_count("task",0.0,&[],500.0,30_000.0),WakeSourceChange::Drained);
        assert_eq!(monitor.armed_timer.unwrap(),ArmedTimer { kind:GoalWaitKind::Monitor,due_at_ms:1500.0,total_ms:1000.0,drain_fire:true });
    }
    #[test] fn channel_drain_during_admission_waits_for_rejection() {
        let mut monitor=MonitorAwareGoalContinuation::default(); monitor.set_wake_source_count("task",1.0,&[],0.0,30_000.0); monitor.arm_timer(GoalWaitKind::Monitor,30_000.0,30_000.0,false,0.0); monitor.hold_direct_input("input",100.0);
        monitor.set_wake_source_count("task",0.0,&[],500.0,30_000.0); assert!(monitor.armed_timer.is_none());
        monitor.resolve_direct_input("input",false,700.0); assert_eq!(monitor.armed_timer.unwrap().due_at_ms,1500.0);
    }
    #[test] fn tool_progress_clears_repetition_window() {
        let mut monitor=MonitorAwareGoalContinuation::default(); monitor.record_assistant_output("same",false); monitor.record_assistant_output("same",false);
        monitor.record_assistant_output("same",true); assert!(monitor.recent_normalized_output_hashes.is_empty());
    }
    #[test] fn output_history_retains_only_three_nonempty_turns() {
        let mut monitor=MonitorAwareGoalContinuation::default(); for text in ["a","b","c","d"," "] { monitor.record_assistant_output(text,false); }
        assert_eq!(monitor.recent_normalized_output_hashes,["b","c","d"].map(crate::continuation::hash_assistant_text));
    }
    #[test] fn user_turn_is_exempt_from_toolless_streak() {
        let mut monitor=MonitorAwareGoalContinuation::default(); monitor.note_user_prompt(); monitor.record_toolless_continuation_turn("g",false);
        assert_eq!(monitor.toolless_continuation_streak,0); monitor.note_continuation_started(); monitor.record_toolless_continuation_turn("g",false); assert_eq!(monitor.toolless_continuation_streak,1);
    }
}
