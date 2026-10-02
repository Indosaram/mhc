use maho_ext_api::CompactionReason;
use crate::state::CompactionExtensionState;
pub const FAILURE_TRIP_THRESHOLD: u64=3;
pub const COOLDOWN_MS: f64=60_000.0;
#[derive(Debug, PartialEq)]
pub struct BreakerNotification {
    pub tripped: bool,
    pub failure_count: u64,
    pub tripped_at: f64,
    pub reason: CompactionReason,
}
pub fn record_success(mut state: CompactionExtensionState) -> CompactionExtensionState {
    state.consecutive_failures=0;
    state.tripped_at=None;
    state
}
pub fn record_failure(mut state: CompactionExtensionState, now: f64, route: Option<CompactionReason>, mut on_trip: Option<&mut dyn FnMut(BreakerNotification)>) -> CompactionExtensionState {
    if state.tripped_at.is_some_and(|at| now>=at+COOLDOWN_MS) { state=record_success(state); }
    state.consecutive_failures=state.consecutive_failures.saturating_add(1);
    if state.consecutive_failures>=FAILURE_TRIP_THRESHOLD && state.tripped_at.is_none() {
        state.tripped_at=Some(now);
        if let Some(notify)=on_trip.as_mut() { notify(BreakerNotification {tripped:true,failure_count:state.consecutive_failures,tripped_at:now,reason:route.unwrap_or(CompactionReason::Threshold)}); }
    }
    state
}
pub fn is_tripped(state:&CompactionExtensionState, now:f64)->bool { state.tripped_at.is_some_and(|at| now<at+COOLDOWN_MS) }
pub fn should_bypass(_state:&CompactionExtensionState, manual:bool, reason:Option<CompactionReason>)->bool { manual || reason==Some(CompactionReason::Manual) }
