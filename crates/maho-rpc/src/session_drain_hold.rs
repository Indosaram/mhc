//! Port of senpi `packages/coding-agent/src/modes/rpc/session-drain-hold.ts`.
//!
//! senpi#3114: each drain pass holds the automatic turns of every open session with no
//! attachment once per session so its running turn is its last and drain can park it when
//! that turn settles.

use std::collections::HashSet;

pub trait DrainHoldHost {
    fn open_unattached_session_ids(&self) -> Vec<String>;
    fn hold_automatic_turns(&mut self, session_id: &str) -> Result<(), String>;
}

pub fn hold_unattended_turns(
    host: &mut dyn DrainHoldHost,
    drain_held_sessions: &mut HashSet<String>,
) {
    for session_id in host.open_unattached_session_ids() {
        if !drain_held_sessions.insert(session_id.clone()) {
            continue;
        }
        if let Err(err) = host.hold_automatic_turns(&session_id) {
            eprintln!("senpi rpc handoff could not hold automatic turns of {session_id}: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockHost {
        sessions: Vec<String>,
        held: Vec<String>,
    }

    impl DrainHoldHost for MockHost {
        fn open_unattached_session_ids(&self) -> Vec<String> {
            self.sessions.clone()
        }
        fn hold_automatic_turns(&mut self, session_id: &str) -> Result<(), String> {
            self.held.push(session_id.to_owned());
            Ok(())
        }
    }

    #[test]
    fn holds_each_unattached_session_once() {
        let mut host = MockHost {
            sessions: vec!["s1".to_owned(), "s2".to_owned()],
            held: Vec::new(),
        };
        let mut held = HashSet::new();
        hold_unattended_turns(&mut host, &mut held);
        hold_unattended_turns(&mut host, &mut held);
        assert_eq!(host.held, vec!["s1", "s2"]);
    }
}
