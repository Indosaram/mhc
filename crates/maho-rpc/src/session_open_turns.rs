//! The turns each session's own records have opened (`agent_start`) and not yet settled
//! (`agent_settled`), counted exactly as the supervisor's lifecycle observer counts them
//! (senpi `rpc/session-open-turns.ts`, including `omo#11419` settling until `agent_idle`).
//!
//! A session sealed mid-turn never publishes the `agent_settled` of the turn its teardown aborts, so the
//! observer's count would stay above zero for good and the host would never idle-exit. The writer asks
//! this ledger, before it seals, how many settles the session still owes, and publishes them.
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub struct SessionOpenTurns {
    open: HashMap<String, u64>,
    /// Sessions whose last run settled but has not reached `agent_idle` yet.
    settling: HashSet<String>,
}

impl SessionOpenTurns {
    pub fn note(&mut self, session_id: &str, record_type: Option<&str>) {
        self.note_with_reason(session_id, record_type, None);
    }

    pub fn note_with_reason(
        &mut self,
        session_id: &str,
        record_type: Option<&str>,
        reason: Option<&str>,
    ) {
        match record_type {
            Some("agent_start") => {
                *self.open.entry(session_id.to_owned()).or_default() += 1;
                self.settling.remove(session_id);
            }
            Some("agent_settled") if reason != Some("session_closed") => {
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
                    // Its `agent_settled` handlers may still run: counted busy until `agent_idle` (omo#11419).
                    self.settling.insert(session_id.to_owned());
                }
            }
            Some("agent_idle") => {
                self.settling.remove(session_id);
            }
            _ => {}
        }
    }

    /// The settles a seal is about to strand; the session's count is forgotten either way.
    pub fn take(&mut self, session_id: &str) -> u64 {
        let open = self.open.remove(session_id).unwrap_or(0);
        let settling = u64::from(self.settling.remove(session_id));
        open + settling
    }

    pub fn clear_session(&mut self, session_id: &str) {
        self.open.remove(session_id);
        self.settling.remove(session_id);
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
        turns.note_with_reason("s", Some("agent_settled"), Some("session_closed"));
        assert_eq!(turns.take("s"), 0);
        turns.note("s", Some("agent_start"));
        turns.note("s", Some("agent_settled"));
        // Settling holds 1 until `agent_idle` arrives.
        assert_eq!(turns.take("s"), 1);
        turns.note("s", Some("agent_start"));
        turns.note("s", Some("agent_settled"));
        turns.note("s", Some("agent_idle"));
        assert_eq!(turns.take("s"), 0);
    }
}
