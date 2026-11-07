use crate::state::CompactionExtensionState;
pub fn increment_accepted(mut state: CompactionExtensionState) -> CompactionExtensionState {
    state.accepted_this_turn = state.accepted_this_turn.saturating_add(1);
    state.accepted_absolute = state.accepted_absolute.saturating_add(1);
    state
}
pub fn increment_ineffective(mut state: CompactionExtensionState) -> CompactionExtensionState {
    state.ineffective_attempts_this_turn = state.ineffective_attempts_this_turn.saturating_add(1);
    state
}
/// How many ineffective automatic compactions one turn may produce before further automatic
/// admission is refused. A compaction that saves nothing leaves the context above the ceiling that
/// asked for it, so the very next provider request asks again; without this bound a summarizer that
/// returns a summary the size of its input loops until the repeated handoffs abort the session.
/// The counter deliberately survives a turn end (see `state::end_turn`): the compaction fires after
/// the turn ended, so clearing it there would refill its own budget. Only a compaction that actually
/// saved context clears it, and an explicit `/compact` is never refused by it.
pub const INEFFECTIVE_ADMISSION_LIMIT: u64 = 4;
#[derive(Debug, PartialEq, Eq)]
pub struct CapDecision {
    pub cancel: bool,
}
/// Successful compactions never consume an admission budget (`accepted_this_turn` and
/// `accepted_absolute` stay telemetry, so a long-lived session keeps compacting). The ineffective
/// counter is the guard: senpi leaves it unread and relies on the circuit breaker alone, which
/// debits rejected attempts only, so an accepted no-op compaction could repeat forever.
pub fn should_reject_by_cap(state: &CompactionExtensionState) -> CapDecision {
    CapDecision {
        cancel: state.ineffective_attempts_this_turn >= INEFFECTIVE_ADMISSION_LIMIT,
    }
}
/// Bookkeeping for an accepted compaction: the counters, the measured yield, and the breaker.
/// An effective compaction refills the ineffective budget, so the cap counts consecutive attempts
/// that left the context where it was rather than every attempt a session ever made.
pub fn record_accepted_compaction(
    state: CompactionExtensionState,
    measured: Option<crate::r#yield::StructuralYield>,
) -> CompactionExtensionState {
    let mut next = increment_accepted(state);
    let Some(measured) = measured else {
        return crate::circuit_breaker::record_success(next);
    };
    next.last_yield = Some(crate::policy::CompactionYield {
        saved_tokens: measured.saved_tokens,
        tokens_before: measured.tokens_before,
    });
    if crate::r#yield::is_ineffective_compaction(measured) {
        next = increment_ineffective(next);
    } else {
        next.ineffective_attempts_this_turn = 0;
    }
    crate::circuit_breaker::record_success(next)
}
