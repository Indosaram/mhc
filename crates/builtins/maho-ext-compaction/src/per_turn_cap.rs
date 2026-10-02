use crate::state::CompactionExtensionState;
pub fn increment_accepted(mut state:CompactionExtensionState)->CompactionExtensionState {
    state.accepted_this_turn=state.accepted_this_turn.saturating_add(1);
    state.accepted_absolute=state.accepted_absolute.saturating_add(1);
    state
}
pub fn increment_ineffective(mut state:CompactionExtensionState)->CompactionExtensionState {
    state.ineffective_attempts_this_turn=state.ineffective_attempts_this_turn.saturating_add(1);
    state
}
#[derive(Debug,PartialEq,Eq)]
pub struct CapDecision {pub cancel:bool}
pub fn should_reject_by_cap(_state:&CompactionExtensionState)->CapDecision {CapDecision {cancel:false}}
