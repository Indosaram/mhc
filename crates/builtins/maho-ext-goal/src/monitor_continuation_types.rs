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
pub type DelayedContinuationKind=crate::wait_progress::GoalWaitKind;
pub type ResumptionChannelCounts=crate::wait_progress::ResumptionChannelCounts;
pub struct GoalContinuationAdmission { pub goal:Goal,pub admitted:bool }
