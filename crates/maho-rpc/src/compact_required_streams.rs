//! senpi `modes/rpc/compact-required-streams.ts`: one minimal delivered-state frontier per
//! physical stream, freezing seed checkpoints when unselected records precede a selected one.

use crate::compact_negotiation::COMPACT_EVENTS_CAPABILITY;
use crate::compact_recovery_state::{
    FreezeCommitVersion, RecoveryCheckpoints, RecoveryLane, RecoveryRoute,
};
use crate::compact_recovery_types::{CheckpointToken, RecoveryCaller, RecoveryRouteKind};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

pub struct RegisteredPeerInfo {
    pub socket_epoch: String,
    pub admissions: HashSet<String>,
}

pub trait RequiredStreamsBackend: Send + Sync {
    fn acceptance_lane(&self, session_id: &str) -> Option<Arc<dyn RecoveryLane>>;
    fn checkpoints(&self) -> RecoveryCheckpoints;
    fn connection_ids(&self) -> Vec<String>;
    fn connection_has(&self, peer: &str, capability: &str) -> bool;
    fn registered_peer(&self, peer: &str) -> Option<RegisteredPeerInfo>;
    fn close_peer(&self, peer: &str, code: &str, detail: &str);
    fn required_sessions(&self, peer: &str) -> Vec<String>;
    fn required_admissions(&self, peer: &str, session_id: &str) -> Vec<String>;
    fn mirror_admissions(&self, peer: &str, session_id: &str, record_type: &str) -> Vec<String>;
}

struct StreamFrontierState {
    epoch: String,
    dirty: bool,
    seed: Option<CheckpointToken>,
    version: Arc<FreezeCommitVersion>,
}

/// One minimal delivered-state frontier per physical stream, never a reducer or omitted-record log.
pub struct CompactRequiredStreams {
    backend: Arc<dyn RequiredStreamsBackend>,
    states: Mutex<HashMap<(String, String), StreamFrontierState>>,
}

impl CompactRequiredStreams {
    pub fn new(backend: Arc<dyn RequiredStreamsBackend>) -> Self {
        Self {
            backend,
            states: Mutex::new(HashMap::new()),
        }
    }

    pub fn sessions(&self, peer: &str) -> Vec<String> {
        self.backend.required_sessions(peer)
    }

    pub fn routes(&self, peer: &str, session_id: &str, record_type: &str) -> Vec<String> {
        let Some(reg) = self.backend.registered_peer(peer) else {
            return Vec::new();
        };
        let checkpoints = self.backend.checkpoints();
        let stream_routes = checkpoints
            .stream(&RecoveryCaller {
                socket_epoch: reg.socket_epoch,
                session_id: session_id.to_owned(),
            })
            .map(|s| s.routes)
            .unwrap_or_default();
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for id in stream_routes {
            if reg.admissions.contains(&id) && seen.insert(id.clone()) {
                out.push(id);
            }
        }
        for id in self
            .backend
            .mirror_admissions(peer, session_id, record_type)
        {
            if seen.insert(id.clone()) {
                out.push(id);
            }
        }
        out
    }

    pub fn prepare(&self, session_id: &str, record: Option<&Value>) {
        let Some(lane) = self.backend.acceptance_lane(session_id) else {
            return;
        };
        if lane.retired() {
            return;
        }
        let checkpoints = self.backend.checkpoints();
        let lane_epoch = lane.session_epoch();

        for peer in self.backend.connection_ids() {
            if !self
                .backend
                .connection_has(&peer, COMPACT_EVENTS_CAPABILITY)
            {
                continue;
            }
            let Some(reg) = self.backend.registered_peer(&peer) else {
                continue;
            };
            let routes = self.backend.required_admissions(&peer, session_id);
            let caller = RecoveryCaller {
                socket_epoch: reg.socket_epoch.clone(),
                session_id: session_id.to_owned(),
            };
            for admission_id in &routes {
                let already_routed = checkpoints
                    .stream(&caller)
                    .is_some_and(|s| s.routes.iter().any(|r| r == admission_id));
                if already_routed {
                    continue;
                }
                let route = RecoveryRoute {
                    socket_epoch: caller.socket_epoch.clone(),
                    admission_id: admission_id.clone(),
                    route_kind: RecoveryRouteKind::Mirror,
                    peer: peer.clone(),
                    session_id: session_id.to_owned(),
                };
                if let Err(failure) = checkpoints.freeze(&route, lane.clone(), true) {
                    self.backend
                        .close_peer(&peer, "checkpoint_unavailable", failure.reason_str());
                    continue;
                }
            }
            let Some(stream) = checkpoints.stream(&caller) else {
                continue;
            };
            if stream.routes.is_empty() {
                continue;
            }
            let key = (peer.clone(), session_id.to_owned());
            let initial_seed = checkpoints.tokens().into_iter().find(|t| {
                t.admission.socket_epoch == caller.socket_epoch
                    && t.admission.session_id == session_id
            });
            let selected = record.map(|record| {
                let record_type = record
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("undefined");
                stream.routes.iter().any(|id| reg.admissions.contains(id))
                    || !self
                        .backend
                        .mirror_admissions(&peer, session_id, record_type)
                        .is_empty()
            });
            let refresh_version = {
                let mut states = self.states.lock().unwrap_or_else(PoisonError::into_inner);
                let state = states
                    .entry(key.clone())
                    .or_insert_with(|| StreamFrontierState {
                        epoch: String::new(),
                        dirty: false,
                        seed: None,
                        version: Arc::new(FreezeCommitVersion::new()),
                    });
                if state.epoch != lane_epoch {
                    state.epoch = lane_epoch.clone();
                    state.dirty = false;
                    state.seed = initial_seed;
                    state.version.advance();
                }
                match selected {
                    None => None,
                    Some(false) => {
                        state.dirty = true;
                        state.version.advance();
                        None
                    }
                    Some(true) if state.dirty => {
                        let expected = state.version.advance();
                        Some((Arc::clone(&state.version), expected))
                    }
                    Some(true) => None,
                }
            };
            if let Some((version, expected)) = refresh_version {
                let route = RecoveryRoute {
                    socket_epoch: caller.socket_epoch.clone(),
                    admission_id: stream.routes[0].clone(),
                    route_kind: if routes.is_empty() {
                        RecoveryRouteKind::Ordinary
                    } else {
                        RecoveryRouteKind::Mirror
                    },
                    peer: peer.clone(),
                    session_id: session_id.to_owned(),
                };
                let committed =
                    checkpoints.freeze_if_current(&route, lane.clone(), &version, &expected);
                match committed {
                    Ok(Some(cp)) => {
                        let accepted = {
                            let mut states =
                                self.states.lock().unwrap_or_else(PoisonError::into_inner);
                            if let Some(state) = states.get_mut(&key)
                                && Arc::ptr_eq(&state.version, &version)
                                && version.matches(&expected)
                            {
                                state.seed = Some(cp.token.clone());
                                state.dirty = false;
                                true
                            } else {
                                false
                            }
                        };
                        if !accepted {
                            checkpoints.release(&cp.token);
                        }
                    }
                    Ok(None) => {}
                    Err(failure) => {
                        self.backend.close_peer(
                            &peer,
                            "checkpoint_unavailable",
                            failure.reason_str(),
                        );
                    }
                }
            }
        }
    }

    pub fn take_seed(&self, peer: &str, session_id: &str) -> Option<CheckpointToken> {
        let mut states = self.states.lock().unwrap_or_else(PoisonError::into_inner);
        states
            .get_mut(&(peer.to_owned(), session_id.to_owned()))
            .and_then(|s| s.seed.take())
    }

    pub fn forget_session(&self, session_id: &str) {
        self.states
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(_, s), state| {
                if s == session_id {
                    state.version.advance();
                    false
                } else {
                    true
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_recovery_state::FrozenLaneCut;
    use crate::compact_settled_messages::SettledRead;
    use crate::session_acceptance_lane::{FrozenAcceptedRoot, RetireReason, SessionAcceptanceLane};
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestLane {
        lane: Mutex<SessionAcceptanceLane>,
        next_key: AtomicU64,
        gen_counter: AtomicU64,
        roots: Mutex<HashMap<usize, FrozenAcceptedRoot>>,
        freeze_probe: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    }

    impl TestLane {
        fn new() -> Self {
            let mut lane = SessionAcceptanceLane::default();
            lane.bootstrap(Default::default(), Some("n1".into()));
            Self {
                lane: Mutex::new(lane),
                next_key: AtomicU64::new(1),
                gen_counter: AtomicU64::new(0),
                roots: Mutex::new(HashMap::new()),
                freeze_probe: Mutex::new(None),
            }
        }
    }

    impl RecoveryLane for TestLane {
        fn session_epoch(&self) -> String {
            self.lane
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .session_epoch()
                .to_owned()
        }
        fn accepted_sequence(&self) -> u64 {
            1
        }
        fn retired(&self) -> bool {
            false
        }
        fn retire_reason(&self) -> Option<RetireReason> {
            None
        }
        fn next_generation(&self) -> Option<u64> {
            Some(self.gen_counter.fetch_add(1, Ordering::SeqCst) + 1)
        }
        fn freeze_cut(&self) -> Option<FrozenLaneCut> {
            let probe = self.freeze_probe.lock().expect("probe").clone();
            if let Some(probe) = probe {
                probe();
            }
            let root = self
                .lane
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .freeze();
            let key = self.next_key.fetch_add(1, Ordering::SeqCst) as usize;
            self.roots
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key, root.clone());
            Some(FrozenLaneCut {
                root_key: key,
                root,
                store: None,
                has_current: true,
            })
        }
        fn release_cut(&self, key: usize) {
            if let Some(r) = self
                .roots
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&key)
            {
                self.lane
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .release(&r);
            }
        }
        fn read_settled(
            &self,
            _: usize,
        ) -> futures::future::BoxFuture<'static, Result<SettledRead, String>> {
            Box::pin(async {
                Ok(SettledRead {
                    settled_messages: vec![],
                    owners: vec![],
                })
            })
        }
        fn read_current(
            &self,
            _: usize,
        ) -> futures::future::BoxFuture<
            'static,
            Result<Value, crate::compact_recovery_bytes::RecoveryBytesError>,
        > {
            Box::pin(async { Ok(json!({})) })
        }
        fn read_components(
            &self,
            _: usize,
        ) -> futures::future::BoxFuture<
            'static,
            Result<Value, crate::compact_recovery_bytes::RecoveryBytesError>,
        > {
            Box::pin(async { Ok(json!({})) })
        }
        fn read_active(
            &self,
            _: usize,
        ) -> futures::future::BoxFuture<
            'static,
            Result<Value, crate::compact_recovery_bytes::RecoveryBytesError>,
        > {
            Box::pin(async { Ok(json!([])) })
        }
        fn read_calls(
            &self,
            _: usize,
        ) -> futures::future::BoxFuture<
            'static,
            Result<Value, crate::compact_recovery_bytes::RecoveryBytesError>,
        > {
            Box::pin(async { Ok(json!({"calls":[]})) })
        }
    }

    struct MockBackend {
        lane: Arc<TestLane>,
        checkpoints: RecoveryCheckpoints,
        mirror_type: Mutex<&'static str>,
        mirror_probe: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    }

    impl RequiredStreamsBackend for MockBackend {
        fn acceptance_lane(&self, _: &str) -> Option<Arc<dyn RecoveryLane>> {
            Some(self.lane.clone())
        }
        fn checkpoints(&self) -> RecoveryCheckpoints {
            self.checkpoints.clone()
        }
        fn connection_ids(&self) -> Vec<String> {
            vec!["p1".into()]
        }
        fn connection_has(&self, _: &str, cap: &str) -> bool {
            cap == COMPACT_EVENTS_CAPABILITY
        }
        fn registered_peer(&self, _: &str) -> Option<RegisteredPeerInfo> {
            Some(RegisteredPeerInfo {
                socket_epoch: "se-1".into(),
                admissions: HashSet::new(),
            })
        }
        fn close_peer(&self, _: &str, _: &str, _: &str) {}
        fn required_sessions(&self, _: &str) -> Vec<String> {
            vec!["s1".into()]
        }
        fn required_admissions(&self, _: &str, _: &str) -> Vec<String> {
            vec!["mir-1".into()]
        }
        fn mirror_admissions(&self, _: &str, _: &str, ty: &str) -> Vec<String> {
            let probe = self.mirror_probe.lock().expect("probe").clone();
            if let Some(probe) = probe {
                probe();
            }
            if ty
                == *self
                    .mirror_type
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
            {
                vec!["mir-1".into()]
            } else {
                Vec::new()
            }
        }
    }

    #[test]
    fn freezes_mirror_route_and_re_freezes_seed_after_unselected_dirty_record() {
        let backend = Arc::new(MockBackend {
            lane: Arc::new(TestLane::new()),
            checkpoints: RecoveryCheckpoints::default(),
            mirror_type: Mutex::new("turn_end"),
            mirror_probe: Mutex::new(None),
        });
        let req = CompactRequiredStreams::new(backend.clone());

        // First prepare freezes initial mirror checkpoint (gen 1)
        req.prepare("s1", Some(&json!({"type":"message_update"})));
        let seed1 = req.take_seed("p1", "s1").expect("initial seed");
        assert_eq!(seed1.admission.generation, 1);
        assert!(req.take_seed("p1", "s1").is_none());

        // Next record is selected ("turn_end"), so dirty flag triggers a fresh seed freeze (gen 2)
        req.prepare("s1", Some(&json!({"type":"turn_end"})));
        let seed2 = req.take_seed("p1", "s1").expect("re-frozen seed");
        assert_eq!(seed2.admission.generation, 2);

        req.forget_session("s1");
    }

    #[test]
    fn frontier_callbacks_run_without_states_lock() {
        let backend = Arc::new(MockBackend {
            lane: Arc::new(TestLane::new()),
            checkpoints: RecoveryCheckpoints::default(),
            mirror_type: Mutex::new("turn_end"),
            mirror_probe: Mutex::new(None),
        });
        let req = Arc::new(CompactRequiredStreams::new(backend.clone()));
        req.prepare("s1", Some(&json!({"type":"message_update"})));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let weak = Arc::downgrade(&req);
        let observed = Arc::clone(&seen);
        let probe: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let req = weak.upgrade().expect("required streams");
            observed
                .lock()
                .expect("observations")
                .push(req.states.try_lock().is_ok());
        });
        *backend.mirror_probe.lock().expect("probe") = Some(Arc::clone(&probe));
        *backend.lane.freeze_probe.lock().expect("probe") = Some(probe);
        req.prepare("s1", Some(&json!({"type":"turn_end"})));
        assert_eq!(*seen.lock().expect("observations"), vec![true, true]);
    }

    #[test]
    fn reentrant_record_invalidates_in_flight_seed() {
        let backend = Arc::new(MockBackend {
            lane: Arc::new(TestLane::new()),
            checkpoints: RecoveryCheckpoints::default(),
            mirror_type: Mutex::new("turn_end"),
            mirror_probe: Mutex::new(None),
        });
        let req = Arc::new(CompactRequiredStreams::new(backend.clone()));
        req.prepare("s1", Some(&json!({"type":"message_update"})));
        req.take_seed("p1", "s1").expect("initial seed");
        let weak = Arc::downgrade(&req);
        let once = Arc::new(std::sync::atomic::AtomicBool::new(false));
        *backend.lane.freeze_probe.lock().expect("probe") = Some(Arc::new(move || {
            if !once.swap(true, Ordering::SeqCst) {
                weak.upgrade()
                    .expect("required streams")
                    .prepare("s1", Some(&json!({"type":"message_update"})));
            }
        }));
        req.prepare("s1", Some(&json!({"type":"turn_end"})));
        assert!(
            req.take_seed("p1", "s1").is_none(),
            "stale seed must not be delivered"
        );
        req.prepare("s1", Some(&json!({"type":"turn_end"})));
        assert!(
            req.take_seed("p1", "s1").is_some(),
            "dirty frontier must be refreshed"
        );
    }

    #[test]
    fn concurrent_refresh_never_returns_retired_seed() {
        let backend = Arc::new(MockBackend {
            lane: Arc::new(TestLane::new()),
            checkpoints: RecoveryCheckpoints::default(),
            mirror_type: Mutex::new("turn_end"),
            mirror_probe: Mutex::new(None),
        });
        let req = Arc::new(CompactRequiredStreams::new(backend.clone()));
        req.prepare("s1", Some(&json!({"type":"message_update"})));
        req.take_seed("p1", "s1").expect("initial seed");
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let once = std::sync::atomic::AtomicBool::new(false);
        *backend.lane.freeze_probe.lock().expect("probe") = Some(Arc::new(move || {
            if !once.swap(true, Ordering::SeqCst) {
                entered_tx.send(()).expect("entered");
                release_rx
                    .lock()
                    .expect("release")
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("release deadline");
            }
        }));
        let first = Arc::clone(&req);
        let worker =
            std::thread::spawn(move || first.prepare("s1", Some(&json!({"type":"turn_end"}))));
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("freeze deadline");
        req.prepare("s1", Some(&json!({"type":"turn_end"})));
        release_tx.send(()).expect("release");
        worker.join().expect("worker");
        let seed = req
            .take_seed("p1", "s1")
            .expect("selected event must have a seed");
        let caller = RecoveryCaller {
            socket_epoch: "se-1".into(),
            session_id: "s1".into(),
        };
        assert!(
            backend.checkpoints.check(&seed, &caller).is_ok(),
            "seed must remain current"
        );
    }
}
