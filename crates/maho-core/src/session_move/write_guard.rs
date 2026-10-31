//! Port of senpi packages/coding-agent/src/core/session-move/write-guard.ts.
//!
//! The session-manager write funnel asks this before every persisted write. It folds only the entries
//! appended since the last call, and starts over when the entry list is replaced by a load or branch.
//! Upstream detects replacement by array identity; Rust compares the slice address, so a reallocated or
//! replaced list is refolded from the start (same verdict, one extra fold).

use serde_json::Value;

use super::ownership::{
    OwnershipStep, SessionMovedAwayError, SessionOwnership, header_session_id, ownership_step,
};

#[derive(Debug, Clone)]
pub struct SessionWriteGuard {
    source: Option<usize>,
    seen: usize,
    session_id: Option<String>,
    verdict: SessionOwnership,
}

impl Default for SessionWriteGuard {
    fn default() -> Self {
        Self {
            source: None,
            seen: 0,
            session_id: None,
            verdict: SessionOwnership::Writable,
        }
    }
}

impl SessionWriteGuard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn assert_writable(
        &mut self,
        entries: &[Value],
        entry: &Value,
        session_file: &str,
    ) -> Result<(), SessionMovedAwayError> {
        let ownership = self.current(entries);
        let SessionOwnership::Refused(refused) = ownership else {
            return Ok(());
        };
        match ownership_step(
            &SessionOwnership::Refused(refused.clone()),
            entry,
            self.session_id.as_deref(),
        ) {
            OwnershipStep::Apply(_) => Ok(()),
            OwnershipStep::None | OwnershipStep::Refused => {
                Err(SessionMovedAwayError::new(session_file, refused))
            }
        }
    }

    pub fn assert_rewritable(
        &mut self,
        entries: &[Value],
        session_file: &str,
    ) -> Result<(), SessionMovedAwayError> {
        match self.current(entries) {
            SessionOwnership::Writable => Ok(()),
            SessionOwnership::Refused(refused) => {
                Err(SessionMovedAwayError::new(session_file, refused))
            }
        }
    }

    pub fn is_writable(&mut self, entries: &[Value]) -> bool {
        matches!(self.current(entries), SessionOwnership::Writable)
    }

    fn current(&mut self, entries: &[Value]) -> SessionOwnership {
        let address = entries.as_ptr() as usize;
        if self.source != Some(address) || entries.len() < self.seen {
            self.source = Some(address);
            self.seen = 0;
            self.session_id = header_session_id(entries).map(str::to_owned);
            self.verdict = SessionOwnership::Writable;
        }
        for entry in &entries[self.seen..] {
            if let OwnershipStep::Apply(next) =
                ownership_step(&self.verdict, entry, self.session_id.as_deref())
            {
                self.verdict = next;
            }
        }
        self.seen = entries.len();
        self.verdict.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_move::ownership::{SESSION_MOVED, SESSION_RECLAIMED, SESSION_RELEASED};
    use serde_json::json;

    fn custom(kind: &str, data: Value) -> Value {
        json!({"type": "custom", "customType": kind, "data": data})
    }

    #[test]
    fn refuses_ordinary_writes_while_moving_and_admits_only_the_same_move() {
        let mut entries = vec![json!({"type": "session", "id": "s-1"})];
        let mut guard = SessionWriteGuard::new();
        assert!(guard.is_writable(&entries));
        entries.push(custom(
            SESSION_RELEASED,
            json!({"reason": "move", "move_id": "m-1", "target": "laptop", "session_id": "s-1"}),
        ));
        let message = json!({"type": "message"});
        assert!(
            guard
                .assert_writable(&entries, &message, "/s.jsonl")
                .is_err()
        );
        assert!(guard.assert_rewritable(&entries, "/s.jsonl").is_err());
        let same = custom(
            SESSION_RECLAIMED,
            json!({"move_id": "m-1", "session_id": "s-1"}),
        );
        assert!(guard.assert_writable(&entries, &same, "/s.jsonl").is_ok());
        let other = custom(
            SESSION_MOVED,
            json!({"move_id": "m-2", "session_id": "s-1"}),
        );
        assert!(guard.assert_writable(&entries, &other, "/s.jsonl").is_err());
        entries.push(same);
        assert!(guard.is_writable(&entries));
    }

    #[test]
    fn starts_over_when_the_entry_list_is_replaced() {
        let moving = vec![
            json!({"type": "session", "id": "s-1"}),
            custom(
                SESSION_RELEASED,
                json!({"reason": "move", "move_id": "m-1", "session_id": "s-1"}),
            ),
        ];
        let mut guard = SessionWriteGuard::new();
        assert!(!guard.is_writable(&moving));
        let fresh = vec![json!({"type": "session", "id": "s-2"})];
        assert!(guard.is_writable(&fresh));
    }
}
