use crate::policy::CompactionYield;
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompactionExtensionState {
    pub consecutive_failures: u64,
    pub tripped_at: Option<f64>,
    pub accepted_this_turn: u64,
    pub ineffective_attempts_this_turn: u64,
    pub accepted_absolute: u64,
    pub last_yield: Option<CompactionYield>,
    pub turn_id: Option<String>,
    pub restoration: Option<serde_json::Value>,
}
pub fn create_initial_state() -> CompactionExtensionState { CompactionExtensionState::default() }
pub fn reset_turn_counter(mut state: CompactionExtensionState, turn_id: &str) -> CompactionExtensionState {
    state.accepted_this_turn=0;
    state.ineffective_attempts_this_turn=0;
    state.turn_id=Some(turn_id.to_owned());
    state
}
