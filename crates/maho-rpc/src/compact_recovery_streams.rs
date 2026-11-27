//! senpi `modes/rpc/compact-recovery-streams.ts`: one peer + session stream (RULING-42 Q2 A):
//! the session incarnation it materializes, its issued generation, the admissions routed through it
//! (RULING-46 item 1) and its current token.

use crate::compact_recovery_types::{RecoveryCaller, RecoveryStreamState};
use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryRoute {
    pub socket_epoch: String,
    pub admission_id: String,
    pub route_kind: crate::compact_recovery_types::RecoveryRouteKind,
    pub peer: String,
    pub session_id: String,
}

impl RecoveryRoute {
    pub fn caller(&self) -> RecoveryCaller {
        RecoveryCaller {
            socket_epoch: self.socket_epoch.clone(),
            session_id: self.session_id.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryStreamRecord {
    pub session_id: String,
    pub session_epoch: String,
    pub generation: u64,
    pub routes: BTreeSet<String>,
    pub current: Option<String>,
}

struct RecoveryStream {
    socket_epoch: String,
    session_id: String,
    session_epoch: String,
    generation: u64,
    routes: BTreeSet<String>,
    current: Option<String>,
}

impl RecoveryStream {
    fn to_record(&self) -> RecoveryStreamRecord {
        RecoveryStreamRecord {
            session_id: self.session_id.clone(),
            session_epoch: self.session_epoch.clone(),
            generation: self.generation,
            routes: self.routes.clone(),
            current: self.current.clone(),
        }
    }
}

fn key_of(caller: &RecoveryCaller) -> (String, String) {
    (caller.socket_epoch.clone(), caller.session_id.clone())
}

/// The stream records of one host: one per peer incarnation and routing session, replaced by a new
/// session incarnation, freed when the peer incarnation or the routing session ends.
#[derive(Default)]
pub struct RecoveryStreams {
    streams: HashMap<(String, String), RecoveryStream>,
    /// Per peer incarnation, each routed admission and how many of its streams route it.
    routed: HashMap<String, HashMap<String, usize>>,
}

impl RecoveryStreams {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn size(&self) -> usize {
        self.streams.len()
    }

    pub fn get(&self, caller: &RecoveryCaller) -> Option<RecoveryStreamRecord> {
        self.streams
            .get(&key_of(caller))
            .map(RecoveryStream::to_record)
    }

    /// Another ordinary hold joins the installed cut without advancing its generation.
    pub fn join(&mut self, route: &RecoveryRoute) {
        let key = key_of(&route.caller());
        if self.streams.contains_key(&key) {
            Self::add_route(
                &mut self.streams,
                &mut self.routed,
                &key,
                &route.admission_id,
            );
        }
    }

    /// Whether any stream of the peer incarnation routes the admission (#5274), without walking sessions.
    pub fn routes(&self, socket_epoch: &str, admission_id: &str) -> bool {
        self.routed
            .get(socket_epoch)
            .is_some_and(|map| map.contains_key(admission_id))
    }

    /// The bounded description a caller may hold: `None` once its peer incarnation or session ended.
    pub fn describe(&self, caller: &RecoveryCaller) -> Option<RecoveryStreamState> {
        let stream = self.streams.get(&key_of(caller))?;
        Some(RecoveryStreamState {
            session_epoch: stream.session_epoch.clone(),
            generation: stream.generation,
            routes: stream.routes.iter().cloned().collect(),
        })
    }

    /// Admits the route's token as the stream's next generation at `session_epoch`; a new session
    /// incarnation replaces the record. Returns the previous current token if any.
    pub fn next(
        &mut self,
        route: &RecoveryRoute,
        session_epoch: &str,
        token_id: &str,
        generation: u64,
    ) -> Option<String> {
        let key = key_of(&route.caller());
        if let Some(existing) = self.streams.get(&key)
            && existing.session_epoch != session_epoch
        {
            self.drop_key(&key);
        }
        let retired = self
            .streams
            .get(&key)
            .and_then(|existing| existing.current.clone());
        let stream = self
            .streams
            .entry(key.clone())
            .or_insert_with(|| RecoveryStream {
                socket_epoch: route.socket_epoch.clone(),
                session_id: route.session_id.clone(),
                session_epoch: session_epoch.to_owned(),
                generation,
                routes: BTreeSet::new(),
                current: None,
            });
        stream.generation = generation;
        stream.current = Some(token_id.to_owned());
        Self::add_route(
            &mut self.streams,
            &mut self.routed,
            &key,
            &route.admission_id,
        );
        retired
    }

    pub fn forget_peer(&mut self, socket_epoch: &str) {
        let keys: Vec<(String, String)> = self
            .streams
            .iter()
            .filter(|(_, s)| s.socket_epoch == socket_epoch)
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            self.drop_key(&key);
        }
    }

    /// The routing session ended for good: its record goes on every peer.
    pub fn forget_session(&mut self, session_id: &str) {
        let keys: Vec<(String, String)> = self
            .streams
            .iter()
            .filter(|(_, s)| s.session_id == session_id)
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            self.drop_key(&key);
        }
    }

    /// An admission ended: its route leaves the stream, the counter stays.
    pub fn retire_route(&mut self, caller: &RecoveryCaller, admission_id: &str) {
        let key = key_of(caller);
        Self::remove_route(&mut self.streams, &mut self.routed, &key, admission_id);
    }

    fn add_route(
        streams: &mut HashMap<(String, String), RecoveryStream>,
        routed: &mut HashMap<String, HashMap<String, usize>>,
        key: &(String, String),
        admission_id: &str,
    ) {
        let Some(stream) = streams.get_mut(key) else {
            return;
        };
        if !stream.routes.insert(admission_id.to_owned()) {
            return;
        }
        let peer_routes = routed.entry(stream.socket_epoch.clone()).or_default();
        *peer_routes.entry(admission_id.to_owned()).or_insert(0) += 1;
    }

    fn remove_route(
        streams: &mut HashMap<(String, String), RecoveryStream>,
        routed: &mut HashMap<String, HashMap<String, usize>>,
        key: &(String, String),
        admission_id: &str,
    ) {
        let Some(stream) = streams.get_mut(key) else {
            return;
        };
        if !stream.routes.remove(admission_id) {
            return;
        }
        if let Some(peer_routes) = routed.get_mut(&stream.socket_epoch) {
            if let Some(count) = peer_routes.get_mut(admission_id) {
                if *count <= 1 {
                    peer_routes.remove(admission_id);
                } else {
                    *count -= 1;
                }
            }
            if peer_routes.is_empty() {
                routed.remove(&stream.socket_epoch);
            }
        }
    }

    fn drop_key(&mut self, key: &(String, String)) {
        let routes: Vec<String> = self
            .streams
            .get(key)
            .map(|s| s.routes.iter().cloned().collect())
            .unwrap_or_default();
        for admission_id in routes {
            Self::remove_route(&mut self.streams, &mut self.routed, key, &admission_id);
        }
        self.streams.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_recovery_types::RecoveryRouteKind;

    fn route(se: &str, sess: &str, adm: &str) -> RecoveryRoute {
        RecoveryRoute {
            socket_epoch: se.into(),
            admission_id: adm.into(),
            route_kind: RecoveryRouteKind::Ordinary,
            peer: "peer-1".into(),
            session_id: sess.into(),
        }
    }

    #[test]
    fn tracks_generations_routes_and_forgets_by_peer_or_session() {
        let mut streams = RecoveryStreams::new();
        let r1 = route("se-1", "s1", "adm-1");
        assert_eq!(streams.next(&r1, "ep-1", "tok-1", 1), None);
        assert!(streams.routes("se-1", "adm-1"));

        let r2 = route("se-1", "s1", "adm-2");
        streams.join(&r2);
        assert!(streams.routes("se-1", "adm-2"));
        assert_eq!(
            streams.next(&r1, "ep-1", "tok-2", 2).as_deref(),
            Some("tok-1")
        );

        // New session epoch replaces stream
        assert_eq!(streams.next(&r1, "ep-2", "tok-3", 1), None);
        assert!(!streams.routes("se-1", "adm-2"));

        streams.retire_route(&r1.caller(), "adm-1");
        assert!(!streams.routes("se-1", "adm-1"));
        assert_eq!(streams.size(), 1);

        streams.forget_session("s1");
        assert_eq!(streams.size(), 0);
    }
}
