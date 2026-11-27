//! senpi `modes/rpc/session-recovery-pins.ts`: pins lane-owned sessions for frozen
//! legacy views (S08a / R3.C1.5) and constructs the shed's recovery cut.

use crate::compact_legacy_view::{LegacyViewSource, ResyncResume, legacy_resync_message};
use crate::compact_recovery_state::{
    FrozenCheckpoint, RecoveryCheckpoints, RecoveryLane, RecoveryRoute, RetainedRoot,
    RouteRetirement,
};
use crate::compact_recovery_types::{
    RecoveryCaller, RecoveryOutcome, RecoveryRouteKind, recovered,
};
use crate::custom_capability::MEDIA_PLACEHOLDERS_CAPABILITY;
use crate::socket_peer_recovery::{ResyncCut, ResyncReason};
use futures::future::BoxFuture;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

/// Slow-peer shed policy (from `socket-peer-shed.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShedMode {
    Capture,
    Drop,
}

/// A lane-owned session pinned for one frozen view (R3.C1.5).
#[derive(Clone)]
pub struct PinnedSession {
    pub session_id: String,
    pub lane: Arc<dyn RecoveryLane>,
    pub session_epoch: String,
    pub outcome: RecoveryOutcome<FrozenCheckpoint>,
}

/// What the pins read of the fanout's peers: a registration's incarnation and the attach ordinals
/// it holds.
pub trait RecoveryPeers: Send + Sync {
    fn socket_epoch(&self, id: &str) -> Option<String>;
    fn ordinals(&self, id: &str) -> Option<HashMap<String, usize>>;
    fn has(&self, id: &str, capability: &str) -> bool;
    fn required(&self, _id: &str) -> Vec<String> {
        Vec::new()
    }
}

/// Legacy replay store interface needed by `SessionRecoveryPins`.
pub trait ReplayStore: Send + Sync {
    fn latest_message(&self, session_id: &str) -> Option<Value>;
    fn track(&self, session_id: &str, frame: &Value);
}

pub fn admission_of(ordinal: usize) -> String {
    format!("attach-{ordinal}")
}

/// Host view options passed into `ResyncCutFactory` for legacy resync frame assembly.
pub struct ResyncViewContext {
    pub reason: ResyncReason,
    pub placeholders: bool,
    pub closed_now: Box<dyn Fn(&str) -> bool + Send + Sync>,
    pub on_release: Box<dyn FnOnce() + Send>,
}

/// Builder bridging unported `legacy-resync-view.ts` (`resyncCut`) cleanly.
pub type ResyncCutBuilder = Arc<
    dyn Fn(
            Vec<String>,
            Box<dyn Fn() -> Vec<String> + Send + Sync>,
            Arc<dyn Fn(&str) -> BoxFuture<'static, ResyncResume> + Send + Sync>,
            ResyncViewContext,
        ) -> ResyncCut
        + Send
        + Sync,
>;

type ReadResyncFn = Arc<
    dyn Fn(
            &dyn RecoveryLane,
            usize,
        ) -> BoxFuture<
            'static,
            Result<Option<crate::compact_legacy_view::ActiveMessageView>, String>,
        > + Send
        + Sync,
>;
type ReadResumeFn = Arc<dyn Fn(&str) -> BoxFuture<'static, ResyncResume> + Send + Sync>;

/// Retained checkpoint wrapper implementing `LegacyViewSource` for `legacy_resync_message`.
struct RetainedCheckpointSource {
    lane: Arc<dyn RecoveryLane>,
    root_key: usize,
    _retained: Mutex<Option<RetainedRoot>>,
    read_resync_fn: ReadResyncFn,
}

impl LegacyViewSource for RetainedCheckpointSource {
    fn recovery_epoch(&self) -> u64 {
        0
    }
    fn is_empty_attach(&self) -> bool {
        false
    }
    fn holds_root(&self) -> bool {
        !self.lane.retired()
    }
    fn read_attach(
        &self,
    ) -> BoxFuture<'_, Result<crate::compact_legacy_view::LegacyViewSnapshot, String>> {
        Box::pin(async { Err("resync source does not support attach snapshot".into()) })
    }
    fn read_resync(
        &self,
    ) -> BoxFuture<'_, Result<Option<crate::compact_legacy_view::ActiveMessageView>, String>> {
        (self.read_resync_fn)(self.lane.as_ref(), self.root_key)
    }
}

fn default_resync_cut(
    sessions: Vec<String>,
    current_sessions: Box<dyn Fn() -> Vec<String> + Send + Sync>,
    read_resume: ReadResumeFn,
    view: ResyncViewContext,
) -> ResyncCut {
    let reason_str = view.reason.as_str().to_owned();
    let closed_now = view.closed_now;
    let on_release = Mutex::new(Some(view.on_release));
    let _ = (sessions, read_resume);
    ResyncCut {
        read: Box::new(move || {
            current_sessions()
                .into_iter()
                .map(|session_id| {
                    let mut v = serde_json::json!({
                        "type": "resync_required",
                        "sessionId": session_id,
                        "reason": reason_str,
                        "message": Value::Null,
                    });
                    if closed_now(&session_id) {
                        v["closed"] = Value::Bool(true);
                    }
                    v
                })
                .collect()
        }),
        release: Some(Box::new(move || {
            if let Some(cb) = on_release
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                cb();
            }
        })),
    }
}

/// The checkpoints a host pins for its peers' legacy views (S08a producers): one per held
/// lane-owned session on the admission (the attach ordinal) that holds it, and the shed's
/// recovery cut read from them.
pub struct SessionRecoveryPins {
    acceptance: Arc<Mutex<HashMap<String, Arc<dyn RecoveryLane>>>>,
    checkpoints: RecoveryCheckpoints,
    replay: Arc<dyn ReplayStore>,
    peers: Arc<dyn RecoveryPeers>,
    shed_mode: ShedMode,
    resync_builder: Option<ResyncCutBuilder>,
}

impl SessionRecoveryPins {
    pub fn new(
        acceptance: Arc<Mutex<HashMap<String, Arc<dyn RecoveryLane>>>>,
        checkpoints: RecoveryCheckpoints,
        replay: Arc<dyn ReplayStore>,
        peers: Arc<dyn RecoveryPeers>,
        shed_mode: ShedMode,
    ) -> Self {
        Self {
            acceptance,
            checkpoints,
            replay,
            peers,
            shed_mode,
            resync_builder: None,
        }
    }

    pub fn with_resync_builder(mut self, builder: ResyncCutBuilder) -> Self {
        self.resync_builder = Some(builder);
        self
    }

    /// One freeze per held lane-owned session, on the admission (the attach ordinal) that holds it
    /// for `id`.
    pub fn pin(&self, id: &str, session_ids: &[String]) -> Vec<PinnedSession> {
        let Some(socket_epoch) = self.peers.socket_epoch(id) else {
            return Vec::new();
        };
        let ordinals = self.peers.ordinals(id);
        let lanes = self
            .acceptance
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut pinned = Vec::new();
        for session_id in session_ids {
            let Some(lane) = lanes.get(session_id).cloned() else {
                continue;
            };
            let ordinal = ordinals
                .as_ref()
                .and_then(|map| map.get(session_id).copied());
            let mirror = self
                .checkpoints
                .stream(&RecoveryCaller {
                    socket_epoch: socket_epoch.clone(),
                    session_id: session_id.clone(),
                })
                .and_then(|s| s.routes.into_iter().next());
            let admission_id = mirror.or_else(|| ordinal.map(admission_of));
            let Some(admission_id) = admission_id else {
                continue;
            };
            let route = RecoveryRoute {
                socket_epoch: socket_epoch.clone(),
                admission_id,
                route_kind: if ordinal.is_none() {
                    RecoveryRouteKind::Mirror
                } else {
                    RecoveryRouteKind::Ordinary
                },
                peer: id.to_owned(),
                session_id: session_id.clone(),
            };
            let session_epoch = lane.session_epoch();
            let outcome = self.checkpoints.freeze(&route, lane.clone(), true);
            pinned.push(PinnedSession {
                session_id: session_id.clone(),
                lane,
                session_epoch,
                outcome,
            });
        }
        pinned
    }

    pub fn unpin(&self, pinned: &[PinnedSession]) {
        for entry in pinned {
            if let Ok(ref cp) = entry.outcome {
                self.checkpoints.release(&cp.token);
            }
        }
    }

    /// The admission holding `session_id` for `id` ends (a detach): its route leaves its stream.
    pub fn retire(&self, id: &str, session_id: &str) {
        let Some(socket_epoch) = self.peers.socket_epoch(id) else {
            return;
        };
        let Some(ordinal) = self
            .peers
            .ordinals(id)
            .and_then(|map| map.get(session_id).copied())
        else {
            return;
        };
        self.checkpoints.retire_route(
            &RecoveryCaller {
                socket_epoch,
                session_id: session_id.to_owned(),
            },
            &admission_of(ordinal),
            RouteRetirement::ClientDetach,
        );
    }

    /// The shed's recovery: checkpoints frozen at the shed, before the actor awaits its drain
    /// (R3.C1.5), as the next generation of each held admission; released when the recovery ends.
    pub fn resync(&self, id: &str, reason: ResyncReason, carried: &[String]) -> ResyncCut {
        let capture = self.shed_mode == ShedMode::Capture;
        let required = if capture {
            self.peers.required(id)
        } else {
            Vec::new()
        };
        let mut attached_set = HashSet::new();
        let mut attached = Vec::new();
        if let Some(ords) = self.peers.ordinals(id) {
            for s in ords.keys() {
                if attached_set.insert(s.clone()) {
                    attached.push(s.clone());
                }
            }
        }
        for s in required {
            if attached_set.insert(s.clone()) {
                attached.push(s);
            }
        }
        let ordinals_snapshot = self.peers.ordinals(id).unwrap_or_default();
        let pinned = self.pin(id, &attached);
        let retained_sources: HashMap<String, RecoveryOutcome<Arc<dyn LegacyViewSource>>> = pinned
            .iter()
            .map(|p| {
                let mapped: RecoveryOutcome<Arc<dyn LegacyViewSource>> = match &p.outcome {
                    Ok(cp) => {
                        let retained = cp.retain();
                        let src: Arc<dyn LegacyViewSource> = Arc::new(RetainedCheckpointSource {
                            lane: cp.lane.clone(),
                            root_key: cp.root_key,
                            _retained: Mutex::new(Some(retained)),
                            read_resync_fn: Arc::new(|lane, key| {
                                let fut = lane.read_active(key);
                                Box::pin(async move {
                                    let val = fut.await.map_err(|e| e.to_string())?;
                                    if val.is_null() {
                                        return Ok(None);
                                    }
                                    serde_json::from_value(val)
                                        .map(Some)
                                        .map_err(|e| e.to_string())
                                })
                            }),
                        });
                        recovered(src)
                    }
                    Err(failure) => Err(failure.clone()),
                };
                (p.session_id.clone(), mapped)
            })
            .collect();

        let peers_closed = self.peers.clone();
        let id_closed = id.to_owned();
        let closed_now: Box<dyn Fn(&str) -> bool + Send + Sync> = Box::new(move |session_id| {
            if capture {
                let in_required = peers_closed
                    .required(&id_closed)
                    .iter()
                    .any(|s| s == session_id);
                let same_ordinal = ordinals_snapshot.get(session_id).is_some_and(|orig| {
                    peers_closed
                        .ordinals(&id_closed)
                        .and_then(|m| m.get(session_id).copied())
                        == Some(*orig)
                });
                !(in_required || same_ordinal)
            } else {
                !peers_closed
                    .ordinals(&id_closed)
                    .is_some_and(|m| m.contains_key(session_id))
            }
        });

        let checkpoints = self.checkpoints.clone();
        let pinned_tokens: Vec<_> = pinned
            .iter()
            .filter_map(|p| p.outcome.as_ref().ok().map(|cp| cp.token.clone()))
            .collect();
        let retained_drop = Arc::new(Mutex::new(Some(retained_sources)));
        let retained_for_release = retained_drop.clone();
        let on_release: Box<dyn FnOnce() + Send> = Box::new(move || {
            retained_for_release
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            for token in &pinned_tokens {
                checkpoints.release(token);
            }
        });

        let sessions = if capture {
            let mut set = HashSet::new();
            let mut merged = Vec::new();
            for s in carried.iter().chain(attached.iter()) {
                if set.insert(s.clone()) {
                    merged.push(s.clone());
                }
            }
            merged
        } else {
            attached
        };

        let sessions_for_current = sessions.clone();
        let peers_current = self.peers.clone();
        let id_current = id.to_owned();
        let current_sessions: Box<dyn Fn() -> Vec<String> + Send + Sync> = if capture {
            Box::new(move || sessions_for_current.clone())
        } else {
            Box::new(move || {
                let mut set = HashSet::new();
                let mut merged = Vec::new();
                for s in &sessions_for_current {
                    if set.insert(s.clone()) {
                        merged.push(s.clone());
                    }
                }
                if let Some(ords) = peers_current.ordinals(&id_current) {
                    for s in ords.keys() {
                        if set.insert(s.clone()) {
                            merged.push(s.clone());
                        }
                    }
                }
                merged
            })
        };

        let acceptance = self.acceptance.clone();
        let replay = self.replay.clone();
        let sources_ref = retained_drop;
        let read_resume: ReadResumeFn = Arc::new(move |session_id| {
            let is_lane = acceptance
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains_key(session_id);
            if is_lane {
                let src = sources_ref
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_ref()
                    .and_then(|map| map.get(session_id).cloned());
                let sid = session_id.to_owned();
                Box::pin(async move { legacy_resync_message(&sid, src).await })
            } else {
                let message = replay.latest_message(session_id);
                Box::pin(async move {
                    ResyncResume {
                        message,
                        frames: Vec::new(),
                    }
                })
            }
        });

        let ctx = ResyncViewContext {
            reason,
            placeholders: self.peers.has(id, MEDIA_PLACEHOLDERS_CAPABILITY),
            closed_now,
            on_release,
        };
        match &self.resync_builder {
            Some(builder) => builder(sessions, current_sessions, read_resume, ctx),
            None => default_resync_cut(sessions, current_sessions, read_resume, ctx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_recovery_state::FrozenLaneCut;
    use crate::compact_settled_messages::SettledRead;
    use crate::session_acceptance_lane::{FrozenAcceptedRoot, RetireReason, SessionAcceptanceLane};
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    struct TestLane {
        lane: Mutex<SessionAcceptanceLane>,
        roots: Mutex<HashMap<usize, FrozenAcceptedRoot>>,
        next_key: AtomicUsize,
        gen_num: AtomicU64,
    }

    impl RecoveryLane for TestLane {
        fn session_epoch(&self) -> String {
            "ep1".into()
        }
        fn accepted_sequence(&self) -> u64 {
            3
        }
        fn retired(&self) -> bool {
            false
        }
        fn retire_reason(&self) -> Option<RetireReason> {
            None
        }
        fn next_generation(&self) -> Option<u64> {
            Some(self.gen_num.fetch_add(1, Ordering::SeqCst) + 1)
        }
        fn freeze_cut(&self) -> Option<FrozenLaneCut> {
            let root = self
                .lane
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .freeze();
            let key = self.next_key.fetch_add(1, Ordering::SeqCst) + 1;
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
        fn read_settled(&self, _: usize) -> BoxFuture<'static, Result<SettledRead, String>> {
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
        ) -> BoxFuture<'static, Result<Value, crate::compact_recovery_bytes::RecoveryBytesError>>
        {
            Box::pin(async { Ok(json!({})) })
        }
        fn read_components(
            &self,
            _: usize,
        ) -> BoxFuture<'static, Result<Value, crate::compact_recovery_bytes::RecoveryBytesError>>
        {
            Box::pin(async { Ok(json!({})) })
        }
        fn read_active(
            &self,
            _: usize,
        ) -> BoxFuture<'static, Result<Value, crate::compact_recovery_bytes::RecoveryBytesError>>
        {
            Box::pin(async { Ok(Value::Null) })
        }
        fn read_calls(
            &self,
            _: usize,
        ) -> BoxFuture<'static, Result<Value, crate::compact_recovery_bytes::RecoveryBytesError>>
        {
            Box::pin(async { Ok(json!({"calls": []})) })
        }
    }

    struct TestPeers;
    impl RecoveryPeers for TestPeers {
        fn socket_epoch(&self, id: &str) -> Option<String> {
            (id == "p1").then(|| "se1".into())
        }
        fn ordinals(&self, id: &str) -> Option<HashMap<String, usize>> {
            (id == "p1").then(|| HashMap::from([("s1".into(), 2)]))
        }
        fn has(&self, _: &str, _: &str) -> bool {
            false
        }
    }

    struct TestReplay;
    impl ReplayStore for TestReplay {
        fn latest_message(&self, _: &str) -> Option<Value> {
            None
        }
        fn track(&self, _: &str, _: &Value) {}
    }

    #[test]
    fn pin_unpin_retire_and_resync_manage_checkpoints() {
        let mut inner_lane = SessionAcceptanceLane::default();
        inner_lane.bootstrap(Default::default(), Some("ep1".into()));
        let lane: Arc<dyn RecoveryLane> = Arc::new(TestLane {
            lane: Mutex::new(inner_lane),
            roots: Mutex::new(HashMap::new()),
            next_key: AtomicUsize::new(0),
            gen_num: AtomicU64::new(0),
        });
        let acceptance = Arc::new(Mutex::new(HashMap::from([("s1".into(), lane)])));
        let checkpoints = RecoveryCheckpoints::new(Arc::new(
            crate::compact_recovery_manifest::RecoveryManifests::default(),
        ));
        let pins = SessionRecoveryPins::new(
            acceptance,
            checkpoints.clone(),
            Arc::new(TestReplay),
            Arc::new(TestPeers),
            ShedMode::Capture,
        );

        let pinned = pins.pin("p1", &["s1".into()]);
        assert_eq!(pinned.len(), 1);
        assert!(pinned[0].outcome.is_ok());
        assert_eq!(checkpoints.status().frozen, 1);

        let cut = pins.resync("p1", ResyncReason::Overflow, &[]);
        let frames = (cut.read)();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["type"], "resync_required");
        if let Some(release) = cut.release {
            release();
        }
        pins.unpin(&pinned);
        pins.retire("p1", "s1");
    }
}
