use crate::types::Goal;
use maho_agent::types::AgentMessage;
use maho_ext_api::ExtensionContext;
use maho_core::agent_abort_provenance::AgentEndEvent;

pub struct AgentEndOptions<'a> {
    pub ctx:&'a ExtensionContext,
    pub goal:Option<&'a Goal>,
    pub messages:&'a [AgentMessage],
}
pub struct SystemAbortOptions<'a> {
    pub agent_end:AgentEndOptions<'a>,
    pub event:&'a AgentEndEvent,
    pub will_retry:bool,
}
pub type ProviderRecoveryOptions<'a>=SystemAbortOptions<'a>;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct ContinuingGoalContinuationVerdict {
    pub prompt:crate::continuation::ContinuationPrompt,
    pub stall_notice:bool,
}
impl TryFrom<crate::continuation::GoalContinuationVerdict> for ContinuingGoalContinuationVerdict {
    type Error=crate::continuation::DenyReason;
    fn try_from(verdict:crate::continuation::GoalContinuationVerdict)->Result<Self,Self::Error> {
        match verdict {
            crate::continuation::GoalContinuationVerdict::Continue { prompt,stall_notice }=>Ok(Self { prompt,stall_notice }),
            crate::continuation::GoalContinuationVerdict::Deny(reason)=>Err(reason),
        }
    }
}
pub type DelayedContinuationKind=crate::wait_progress::GoalWaitKind;
pub type ResumptionChannelCounts=crate::wait_progress::ResumptionChannelCounts;
pub struct GoalContinuationAdmission { pub goal:Goal,pub admitted:bool }
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn narrowed_verdict_carries_continue_payload_and_rejects_denial() {
        use crate::continuation::{GoalContinuationVerdict,ContinuationPrompt,DenyReason};
        assert_eq!(ContinuingGoalContinuationVerdict::try_from(GoalContinuationVerdict::Continue { prompt:ContinuationPrompt::Minimal,stall_notice:true }).unwrap(),ContinuingGoalContinuationVerdict { prompt:ContinuationPrompt::Minimal,stall_notice:true });
        assert_eq!(ContinuingGoalContinuationVerdict::try_from(GoalContinuationVerdict::Deny(DenyReason::SingleFlight)),Err(DenyReason::SingleFlight));
    }
}
