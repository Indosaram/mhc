//! The supervisor's view of its public-socket clients, split by what they mean for idle exit (senpi
//! `host-client-occupancy.ts`). A socket host applies the same view to its own connections for its empty-exit window.
//!
//! A client is UNCLASSIFIED until its first request line arrives, an OBSERVER while every line it has sent is an
//! observing read (`host_observe_request`), and ATTACHED from the first line that is anything else - or from a first
//! line too long to be a read. Only attached clients are occupancy. A client whose first such line names this host's
//! own generation as its origin is DEPENDENT instead: one of the host's own sessions, which never holds it open. An
//! unclassified client neither counts nor lets the host exit: its first line may be the readiness probe of an ensure
//! that is about to open a session.
//!
//! ADAPTATION: upstream subscribes to each `Duplex`'s `data` event and destroys the streams itself. Here a client is
//! any hashable key; the transport feeds every chunk it reads through [`ClientOccupancy::data`] until the client is
//! classified, and [`ClientOccupancy::destroy_all`] hands back the clients the caller must close.
use crate::host_observe_request::{is_observing_request, request_origin_host_instance};
use std::collections::{HashMap, HashSet};
use std::hash::Hash;

const MAX_CLASSIFIED_LINE_BYTES: usize = 64 * 1024;
const NEWLINE: u8 = b'\n';

pub struct ClientOccupancy<K> {
    /// Admitted clients still being classified, with the bytes of their unfinished line.
    clients: HashMap<K, Vec<u8>>,
    observers: HashSet<K>,
    attached: HashSet<K>,
    dependents: HashSet<K>,
    on_attach: Box<dyn FnMut() + Send>,
    own_instance: Option<String>,
    closed: bool,
}

impl<K: Eq + Hash + Clone> ClientOccupancy<K> {
    pub fn new(on_attach: impl FnMut() + Send + 'static, own_instance: Option<String>) -> Self {
        Self {
            clients: HashMap::new(),
            observers: HashSet::new(),
            attached: HashSet::new(),
            dependents: HashSet::new(),
            on_attach: Box::new(on_attach),
            own_instance,
            closed: false,
        }
    }

    pub fn attached_count(&self) -> usize {
        self.attached.len()
    }

    pub fn unclassified_count(&self) -> usize {
        self.clients
            .keys()
            .filter(|client| !self.observers.contains(*client))
            .count()
    }

    /// Starts classifying `client`; `false` once [`Self::destroy_all`] ran (the caller closes the client).
    pub fn admit(&mut self, client: K) -> bool {
        if self.closed {
            return false;
        }
        self.clients.insert(client, Vec::new());
        true
    }

    /// One chunk the client sent. Ignored once the client is attached or dependent (upstream stops listening).
    pub fn data(&mut self, client: &K, chunk: &[u8]) {
        let Some(pending) = self.clients.get_mut(client) else {
            return;
        };
        pending.extend_from_slice(chunk);
        while let Some(newline) = pending.iter().position(|byte| *byte == NEWLINE) {
            let line = String::from_utf8_lossy(&pending[..newline]).into_owned();
            pending.drain(..=newline);
            if !is_observing_request(&line) {
                if self.own_instance.is_some()
                    && request_origin_host_instance(&line) == self.own_instance
                {
                    self.depend(client);
                } else {
                    self.attach(client);
                }
                return;
            }
            self.observers.insert(client.clone());
        }
        if self
            .clients
            .get(client)
            .is_some_and(|pending| pending.len() > MAX_CLASSIFIED_LINE_BYTES)
        {
            self.attach(client);
        }
    }

    fn attach(&mut self, client: &K) {
        self.clients.remove(client);
        self.observers.remove(client);
        self.attached.insert(client.clone());
        (self.on_attach)();
    }

    fn depend(&mut self, client: &K) {
        self.clients.remove(client);
        self.observers.remove(client);
        self.dependents.insert(client.clone());
    }

    /// Returns whether this peer left without ever proving it was an observing read.
    pub fn release(&mut self, client: &K) -> bool {
        let unclassified = self.clients.contains_key(client) && !self.observers.contains(client);
        self.clients.remove(client);
        self.observers.remove(client);
        self.attached.remove(client);
        self.dependents.remove(client);
        unclassified
    }

    /// Admits nothing more and returns every client still held, for the caller to close.
    pub fn destroy_all(&mut self) -> Vec<K> {
        self.closed = true;
        self.clients
            .keys()
            .chain(&self.observers)
            .chain(&self.attached)
            .chain(&self.dependents)
            .cloned()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_lifecycle::{IdleExitDecider, IdleExitDecision};
    use crate::host_observe_request::ORIGIN_HOST_INSTANCE_FIELD;
    use serde_json::{Value, json};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const OWN: &str = "own-generation";

    fn send(occupancy: &mut ClientOccupancy<u32>, client: u32, request: Value) {
        occupancy.data(&client, format!("{request}\n").as_bytes());
    }

    fn counting(own: Option<&str>) -> (ClientOccupancy<u32>, Arc<AtomicUsize>) {
        let attached = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&attached);
        let occupancy = ClientOccupancy::new(
            move || {
                counter.fetch_add(1, Ordering::SeqCst);
            },
            own.map(str::to_owned),
        );
        (occupancy, attached)
    }

    /// rpc-host-client-origin.test.ts "counts a client from its own generation as neither attached nor unclassified".
    #[test]
    fn a_client_from_its_own_generation_is_neither_attached_nor_unclassified() {
        let (mut occupancy, _) = counting(Some(OWN));
        occupancy.admit(1);
        send(
            &mut occupancy,
            1,
            json!({"type":"list_sessions",ORIGIN_HOST_INSTANCE_FIELD:OWN}),
        );
        assert_eq!(
            (occupancy.attached_count(), occupancy.unclassified_count()),
            (0, 0)
        );
    }

    /// rpc-host-client-origin.test.ts "attaches %s": another generation, an unmarked client, a host that knows no
    /// generation.
    #[test]
    fn attaches_another_generation_an_unmarked_client_and_any_client_of_an_unnamed_host() {
        for (request, own) in [
            (
                json!({"type":"list_sessions",ORIGIN_HOST_INSTANCE_FIELD:"other"}),
                Some(OWN),
            ),
            (json!({"type":"list_sessions"}), Some(OWN)),
            (
                json!({"type":"list_sessions",ORIGIN_HOST_INSTANCE_FIELD:OWN}),
                None,
            ),
        ] {
            let (mut occupancy, attached) = counting(own);
            occupancy.admit(1);
            send(&mut occupancy, 1, request);
            assert_eq!(occupancy.attached_count(), 1);
            assert_eq!(attached.load(Ordering::SeqCst), 1);
        }
    }

    /// rpc-host-client-origin.test.ts "releases a dependent without reporting an unclassified admission".
    #[test]
    fn releases_a_dependent_without_reporting_an_unclassified_admission() {
        let (mut occupancy, _) = counting(Some(OWN));
        occupancy.admit(1);
        send(
            &mut occupancy,
            1,
            json!({"type":"prompt",ORIGIN_HOST_INSTANCE_FIELD:OWN}),
        );
        assert!(!occupancy.release(&1));
        assert_eq!(occupancy.unclassified_count(), 0);
    }

    /// rpc-host-client-origin.test.ts "keeps an observing read an observer whatever its origin".
    #[test]
    fn an_observing_read_stays_an_observer() {
        let (mut occupancy, _) = counting(Some(OWN));
        occupancy.admit(1);
        send(
            &mut occupancy,
            1,
            json!({"type":"get_protocol_info","observe":true}),
        );
        assert_eq!(
            (occupancy.attached_count(), occupancy.unclassified_count()),
            (0, 0)
        );
    }

    /// host-lifecycle.test.ts "observing read keeps attachedCount 0 and never resets the idle window" (virtual clock).
    #[test]
    fn observing_reads_never_reset_the_idle_window() {
        let mut decider = IdleExitDecider::new(1_000.0);
        let (mut occupancy, resets) = counting(None);
        let mut now = 0.0;
        assert_eq!(
            decider.update(occupancy.attached_count() as u64, 0, now),
            IdleExitDecision::Idle
        );
        occupancy.admit(1);
        for _ in 0..5 {
            now += 250.0;
            send(
                &mut occupancy,
                1,
                json!({"type":"get_state","sessionId":"s1","observe":true}),
            );
            send(
                &mut occupancy,
                1,
                json!({"type":"get_entries","sessionId":"s1","tail":50,"observe":true}),
            );
            if now < 1_000.0 {
                assert_eq!(
                    decider.update(occupancy.attached_count() as u64, 0, now),
                    IdleExitDecision::Idle
                );
            }
        }
        assert_eq!(resets.load(Ordering::SeqCst), 0);
        assert_eq!(
            (occupancy.attached_count(), occupancy.unclassified_count()),
            (0, 0)
        );
        assert_eq!(
            decider.update(occupancy.attached_count() as u64, 0, now),
            IdleExitDecision::Exit
        );
    }

    /// host-lifecycle.test.ts "attaches the connection, and resets the window, from its first unmarked read".
    #[test]
    fn the_first_unmarked_read_attaches() {
        let mut decider = IdleExitDecider::new(1_000.0);
        let (mut occupancy, resets) = counting(None);
        occupancy.admit(1);
        send(
            &mut occupancy,
            1,
            json!({"type":"get_state","sessionId":"s1","observe":true}),
        );
        send(
            &mut occupancy,
            1,
            json!({"type":"get_state","sessionId":"s1"}),
        );
        assert_eq!(resets.load(Ordering::SeqCst), 1);
        assert_eq!(occupancy.attached_count(), 1);
        assert_eq!(
            decider.update(occupancy.attached_count() as u64, 0, 5_000.0),
            IdleExitDecision::Active
        );
    }

    /// An unclassified peer that leaves is reported; a line split across chunks still classifies; an oversized first
    /// line attaches; nothing is admitted after `destroy_all`.
    #[test]
    fn split_lines_oversized_lines_release_and_destroy_all() {
        let (mut occupancy, attached) = counting(None);
        occupancy.admit(1);
        assert_eq!(occupancy.unclassified_count(), 1);
        assert!(occupancy.release(&1));
        occupancy.admit(2);
        occupancy.data(&2, br#"{"type":"get_protocol_info","#);
        assert_eq!(occupancy.unclassified_count(), 1);
        occupancy.data(&2, b"\"observe\":true}\n");
        assert_eq!(occupancy.unclassified_count(), 0);
        occupancy.admit(3);
        occupancy.data(&3, &vec![b'x'; MAX_CLASSIFIED_LINE_BYTES + 1]);
        assert_eq!(occupancy.attached_count(), 1);
        assert_eq!(attached.load(Ordering::SeqCst), 1);
        let mut held = occupancy.destroy_all();
        held.sort();
        assert_eq!(held, [2, 3]);
        assert!(!occupancy.admit(4));
    }
}
