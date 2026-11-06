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
    pub restoration: Option<crate::restoration_tracker::RestorationTrackerState>,
}
pub fn create_initial_state() -> CompactionExtensionState { CompactionExtensionState::default() }
/// A turn ended. `accepted_this_turn` is per-turn telemetry, but the ineffective count is not: the
/// extension compacts after the turn has ended, and a compaction that saved nothing makes the next
/// turn end the same way, so clearing it here would let that loop reset its own budget forever. An
/// effective compaction is what refills the budget.
pub fn end_turn(mut state: CompactionExtensionState) -> CompactionExtensionState {
    state.accepted_this_turn=0;
    state
}
pub fn reset_turn_counter(mut state: CompactionExtensionState, turn_id: &str) -> CompactionExtensionState {
    state.accepted_this_turn=0;
    state.ineffective_attempts_this_turn=0;
    state.turn_id=Some(turn_id.to_owned());
    state
}
