use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Absent, Starting, IdleSynced, TurnWaiting, TurnSent, TurnClaimed,
    TurnStreaming, TurnResultSeen, Tainted, Closing, Closed, Broken,
}

impl SessionState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "ABSENT", Self::Starting => "STARTING", Self::IdleSynced => "IDLE_SYNCED",
            Self::TurnWaiting => "TURN_WAITING", Self::TurnSent => "TURN_SENT", Self::TurnClaimed => "TURN_CLAIMED",
            Self::TurnStreaming => "TURN_STREAMING", Self::TurnResultSeen => "TURN_RESULT_SEEN", Self::Tainted => "TAINTED",
            Self::Closing => "CLOSING", Self::Closed => "CLOSED", Self::Broken => "BROKEN",
        }
    }
    const fn allowed(self) -> &'static [Self] {
        match self {
            Self::Absent => &[Self::Starting],
            Self::Starting => &[Self::IdleSynced, Self::Tainted, Self::Closing, Self::Broken],
            Self::IdleSynced => &[Self::TurnWaiting, Self::Tainted, Self::Closing, Self::Broken],
            Self::TurnWaiting => &[Self::TurnSent, Self::Tainted, Self::Closing, Self::Broken],
            Self::TurnSent => &[Self::TurnClaimed, Self::Tainted, Self::Closing, Self::Broken],
            Self::TurnClaimed => &[Self::TurnStreaming, Self::Tainted, Self::Closing, Self::Broken],
            Self::TurnStreaming => &[Self::TurnResultSeen, Self::Tainted, Self::Closing, Self::Broken],
            Self::TurnResultSeen => &[Self::IdleSynced, Self::Tainted, Self::Closing, Self::Broken],
            Self::Tainted => &[Self::Closing, Self::Broken], Self::Closing => &[Self::Closed, Self::Broken],
            Self::Closed => &[], Self::Broken => &[Self::Closing, Self::Closed],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IllegalTransition { pub from: SessionState, pub to: SessionState }
impl fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Illegal session state transition: {} -> {}", self.from.as_str(), self.to.as_str())
    }
}
impl std::error::Error for IllegalTransition {}

pub fn transition_session_state(state: &mut SessionState, next: SessionState) -> Result<(), IllegalTransition> {
    if !state.allowed().contains(&next) { return Err(IllegalTransition { from: *state, to: next }); }
    *state = next;
    Ok(())
}

macro_rules! transitions {
    ($($name:ident => $state:ident),* $(,)?) => {
        $(pub fn $name(state: &mut SessionState) -> Result<(), IllegalTransition> {
            transition_session_state(state, SessionState::$state)
        })*
    };
}
transitions!(
    transition_to_starting => Starting, transition_to_idle_synced => IdleSynced,
    transition_to_turn_waiting => TurnWaiting, transition_to_turn_sent => TurnSent,
    transition_to_turn_claimed => TurnClaimed, transition_to_turn_streaming => TurnStreaming,
    transition_to_turn_result_seen => TurnResultSeen, transition_to_tainted => Tainted,
    transition_to_closing => Closing, transition_to_closed => Closed, transition_to_broken => Broken,
);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_turn_and_shutdown() {
        let mut state = SessionState::Absent;
        transition_to_starting(&mut state).unwrap();
        transition_to_idle_synced(&mut state).unwrap();
        transition_to_turn_waiting(&mut state).unwrap();
        transition_to_turn_sent(&mut state).unwrap();
        transition_to_turn_claimed(&mut state).unwrap();
        transition_to_turn_streaming(&mut state).unwrap();
        transition_to_turn_result_seen(&mut state).unwrap();
        transition_to_idle_synced(&mut state).unwrap();
        transition_to_closing(&mut state).unwrap();
        transition_to_closed(&mut state).unwrap();
        assert_eq!(state, SessionState::Closed);
    }
    #[test]
    fn invalid_transition_preserves_state() {
        let mut state = SessionState::IdleSynced;
        let error = transition_to_turn_streaming(&mut state).unwrap_err();
        assert_eq!(state, SessionState::IdleSynced);
        assert_eq!(error, IllegalTransition { from: SessionState::IdleSynced, to: SessionState::TurnStreaming });
        assert_eq!(error.to_string(), "Illegal session state transition: IDLE_SYNCED -> TURN_STREAMING");
    }
    #[test]
    fn transition_matrix_matches_upstream() {
        use SessionState::*;
        let states = [Absent, Starting, IdleSynced, TurnWaiting, TurnSent, TurnClaimed, TurnStreaming, TurnResultSeen, Tainted, Closing, Closed, Broken];
        let targets: [&[SessionState]; 12] = [
            &[Starting], &[IdleSynced, Tainted, Closing, Broken], &[TurnWaiting, Tainted, Closing, Broken],
            &[TurnSent, Tainted, Closing, Broken], &[TurnClaimed, Tainted, Closing, Broken],
            &[TurnStreaming, Tainted, Closing, Broken], &[TurnResultSeen, Tainted, Closing, Broken],
            &[IdleSynced, Tainted, Closing, Broken], &[Closing, Broken], &[Closed, Broken], &[], &[Closing, Closed],
        ];
        for (from, allowed) in states.iter().zip(targets) {
            for to in states {
                let mut state = *from;
                let result = transition_session_state(&mut state, to);
                assert_eq!(result.is_ok(), allowed.contains(&to));
                assert_eq!(state, if result.is_ok() { to } else { *from });
            }
        }
    }
}
