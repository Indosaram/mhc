use crate::{types::{Goal,GoalStatus},terminal_provider_error::{did_terminal_policy_rejection_end_turn,did_terminal_provider_error_end_turn}};
use maho_core::agent_abort_provenance::AgentEndEvent;
use maho_ext_api::AbortSource;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum GoalAgentEndRoute { PolicyBlock,SystemAbort,ProviderFailure,AgentEnd }
pub fn goal_agent_end_route(goal:Option<&Goal>,event:&AgentEndEvent)->GoalAgentEndRoute {
    if goal.is_some_and(|goal|goal.status==GoalStatus::Active) && did_terminal_policy_rejection_end_turn(event) { return GoalAgentEndRoute::PolicyBlock; }
    if event.aborted && event.abort_source==Some(AbortSource::System) { return GoalAgentEndRoute::SystemAbort; }
    if crate::last_assistant_message::last_assistant_message(&event.messages).is_some_and(crate::continuation::is_malformed_tool_use_turn) || did_terminal_provider_error_end_turn(event) { return GoalAgentEndRoute::ProviderFailure; }
    GoalAgentEndRoute::AgentEnd
}
#[cfg(test)] mod tests {
    use super::*;
    fn event()->AgentEndEvent { AgentEndEvent { messages:Vec::new(),aborted:false,will_retry:false,abort_source:None } }
    #[test] fn system_owned_abort_uses_system_recovery_even_without_goal() {
        let mut event=event(); event.aborted=true; event.abort_source=Some(AbortSource::System);
        assert_eq!(goal_agent_end_route(None,&event),GoalAgentEndRoute::SystemAbort);
    }
    #[test] fn user_owned_abort_does_not_take_system_recovery() {
        let mut event=event(); event.aborted=true; event.abort_source=Some(AbortSource::User);
        assert_eq!(goal_agent_end_route(None,&event),GoalAgentEndRoute::AgentEnd);
    }
    #[test] fn ordinary_empty_end_uses_normal_continuation_path() { assert_eq!(goal_agent_end_route(None,&event()),GoalAgentEndRoute::AgentEnd); }
}
