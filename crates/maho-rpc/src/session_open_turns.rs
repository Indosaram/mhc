//! The turns each session's own records have opened (`agent_start`) and not yet settled
//! (`agent_settled`), counted exactly as the supervisor's lifecycle observer counts them
//! (senpi `rpc/session-open-turns.ts`).
//!
//! A session sealed mid-turn never publishes the `agent_settled` of the turn its teardown aborts, so the
//! observer's count would stay above zero for good and the host would never idle-exit. The writer asks
//! this ledger, before it seals, how many settles the session still owes, and publishes them.
use std::collections::HashMap;

#[derive(Default)]
pub struct SessionOpenTurns {
    open: HashMap<String, u64>,
}

impl SessionOpenTurns {
    pub fn note(&mut self, session_id: &str, record_type: Option<&str>) {
        match record_type {
            Some("agent_start") => *self.open.entry(session_id.to_owned()).or_default() += 1,
            Some("agent_settled") => {
                let left = self
                    .open
                    .get(session_id)
                    .copied()
                    .unwrap_or(0)
                    .saturating_sub(1);
                if left > 0 {
                    self.open.insert(session_id.to_owned(), left);
                } else {
                    self.open.remove(session_id);
                }
            }
            _ => {}
        }
    }

    /// The settles a seal is about to strand; the session's count is forgotten either way.
    pub fn take(&mut self, session_id: &str) -> u64 {
        self.open.remove(session_id).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_open_turns_and_forgets_on_take() {
        let mut turns = SessionOpenTurns::default();
        turns.note("s", Some("agent_start"));
        turns.note("s", Some("agent_start"));
        turns.note("s", Some("agent_settled"));
        turns.note("s", Some("message_update"));
        assert_eq!(turns.take("s"), 1);
        assert_eq!(turns.take("s"), 0);
    }

    #[test]
    fn an_unmatched_settle_never_goes_negative() {
        let mut turns = SessionOpenTurns::default();
        turns.note("s", Some("agent_settled"));
        turns.note("s", Some("agent_start"));
        assert_eq!(turns.take("s"), 1);
    }
}
