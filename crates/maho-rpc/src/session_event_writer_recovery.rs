//! Port of senpi `packages/coding-agent/src/modes/rpc/session-event-writer-recovery.ts`.
//!
//! Peer admission routing, baseline installation, open supersession, and detach retirement
//! hooks for compact recovery (`S08`/`S11`/`S13`/`S16`).

use crate::route_retire_reasons::RouteRetirement;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct OpenedRoute {
    pub admission_id: String,
    pub retired: bool,
}

#[derive(Debug, Default)]
pub struct PeerAdmissionState {
    pub admissions: HashSet<String>,
    pub opened: HashMap<String, OpenedRoute>,
    pub subscriptions: HashMap<String, String>,
    pub mirror_owners: HashMap<String, String>,
    pub deferred_retires: HashMap<String, Vec<String>>,
}

impl PeerAdmissionState {
    pub fn register_admission(&mut self, admission_id: &str) {
        self.admissions.insert(admission_id.to_owned());
    }

    pub fn record_subscription(&mut self, subscription_id: &str, admission_id: &str) {
        self.subscriptions
            .insert(subscription_id.to_owned(), admission_id.to_owned());
    }

    pub fn remove_subscription(&mut self, subscription_id: &str) {
        self.subscriptions.remove(subscription_id);
    }

    /// RULING-117: replaces a previous open's route on this socket for `session_id` if different.
    pub fn supersede_open(&mut self, session_id: &str, admission_id: &str) -> Option<String> {
        let prev = self.opened.insert(
            session_id.to_owned(),
            OpenedRoute {
                admission_id: admission_id.to_owned(),
                retired: false,
            },
        );
        prev.and_then(|old| {
            if old.admission_id != admission_id && !old.retired {
                Some(old.admission_id)
            } else {
                None
            }
        })
    }

    /// RULING-118: takes the opened route on detach so it retires once.
    pub fn take_opened_on_detach(&mut self, session_id: &str) -> Option<String> {
        self.opened.remove(session_id).map(|o| o.admission_id)
    }
}

#[derive(Default)]
pub struct SessionEventWriterRecovery {
    peers: HashMap<String, PeerAdmissionState>,
}

impl SessionEventWriterRecovery {
    pub fn peer_mut(&mut self, peer: &str) -> &mut PeerAdmissionState {
        self.peers.entry(peer.to_owned()).or_default()
    }

    pub fn peer(&self, peer: &str) -> Option<&PeerAdmissionState> {
        self.peers.get(peer)
    }

    pub fn drop_peer(&mut self, peer: &str) -> Option<PeerAdmissionState> {
        self.peers.remove(peer)
    }

    pub fn retire_reason_label(retirement: RouteRetirement) -> &'static str {
        retirement.reason().as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supersede_open_returns_prior_distinct_admission_id() {
        let mut recovery = SessionEventWriterRecovery::default();
        let peer = recovery.peer_mut("p1");
        assert_eq!(peer.supersede_open("s1", "adm-1"), None);
        assert_eq!(peer.supersede_open("s1", "adm-1"), None);
        assert_eq!(peer.supersede_open("s1", "adm-2").as_deref(), Some("adm-1"));
        assert_eq!(peer.take_opened_on_detach("s1").as_deref(), Some("adm-2"));
    }
}
