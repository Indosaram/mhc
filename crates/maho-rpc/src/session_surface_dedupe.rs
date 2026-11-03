//! F13: one `loaded_surfaces_changed` per connection per inventory change, never one per session
//! (senpi `rpc/session-surface-dedupe.ts`).
//!
//! Per connection it remembers, for every session, the digest that session was last announced at and the
//! digest it arrived from. A session's change is told only when no OTHER session already announced the
//! same transition: sessions moving together share one event, but a session converging onto a digest
//! from a different starting point still announces its own change.
use std::collections::HashMap;

struct Announced {
    last: String,
    previous: Option<String>,
}

#[derive(Default)]
pub struct SurfaceDigestDedupe {
    connections: HashMap<String, Vec<(String, Announced)>>,
}

impl SurfaceDigestDedupe {
    /// Record one session's move from `previous` to `digest` for a connection; true when the connection
    /// must hear about it. `previous` is the session's real prior digest, never this connection's memory.
    pub fn announce(
        &mut self,
        connection_id: &str,
        session_id: &str,
        previous: Option<&str>,
        digest: &str,
    ) -> bool {
        let sessions = self
            .connections
            .entry(connection_id.to_owned())
            .or_default();
        if previous == Some(digest) {
            return false;
        }
        let tell = !sessions.iter().any(|(other, announced)| {
            other != session_id
                && announced.last == digest
                && announced.previous.as_deref() == previous
        });
        let record = Announced {
            last: digest.to_owned(),
            previous: previous.map(str::to_owned),
        };
        match sessions.iter_mut().find(|(other, _)| other == session_id) {
            Some((_, slot)) => *slot = record,
            None => sessions.push((session_id.to_owned(), record)),
        }
        tell
    }

    pub fn drop_connection(&mut self, connection_id: &str) {
        self.connections.remove(connection_id);
    }

    pub fn forget_session(&mut self, session_id: &str) {
        for sessions in self.connections.values_mut() {
            sessions.retain(|(other, _)| other != session_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_moving_together_share_one_event() {
        let mut dedupe = SurfaceDigestDedupe::default();
        assert!(dedupe.announce("c", "a", Some("d0"), "d1"));
        assert!(!dedupe.announce("c", "b", Some("d0"), "d1"));
        assert!(dedupe.announce("c", "x", Some("other"), "d1"));
        assert!(!dedupe.announce("c", "a", Some("d1"), "d1"));
    }

    #[test]
    fn dropped_connections_and_forgotten_sessions_announce_again() {
        let mut dedupe = SurfaceDigestDedupe::default();
        assert!(dedupe.announce("c", "a", None, "d1"));
        dedupe.forget_session("a");
        assert!(dedupe.announce("c", "b", None, "d1"));
        dedupe.drop_connection("c");
        assert!(dedupe.announce("c", "a", None, "d1"));
    }
}
