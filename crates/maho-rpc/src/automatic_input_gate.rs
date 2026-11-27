use maho_core::agent_session::AgentSession;

pub fn hold_automatic_input(session: &AgentSession, message: &str, interactive_user: bool) -> bool {
    !interactive_user && session.note_automatic_input(message)
}

/// Variant taking `expected_turn_id` (`automatic-input-gate.ts`): a notice is delivered in a later turn,
/// which a steer that names its turn must never reach.
pub fn hold_automatic_input_with_turn(
    session: &AgentSession,
    message: &str,
    interactive_user: bool,
    expected_turn_id: Option<&str>,
) -> bool {
    if interactive_user || expected_turn_id.is_some() {
        return false;
    }
    session.note_automatic_input(message)
}

/// Whether `hold_automatic_input` would keep this connection's input as a notice right now, without storing anything.
pub fn automatic_input_would_be_held(session: &AgentSession, interactive_user: bool) -> bool {
    !interactive_user
        && session.holds().iter().any(|record| {
            (record.kind == maho_core::engine_turn_hold::HoldKind::SessionHold
                && record.key != maho_core::engine_turn_hold::INTERRUPTED_RUN_HOLD_KEY)
                || record.kind == maho_core::engine_turn_hold::HoldKind::InheritedHold
        })
}
