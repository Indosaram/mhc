//! senpi `modes/rpc/compact-admission-routing.ts`: per-admission routing actor wrapper over
//! `SocketEventSinkActor`. Every enqueue path uses the same route; IDs are a subset of the
//! bounded epoch attempt index.

use crate::compact_admission_reply::{ReplyAdmission, reply_admission};
use crate::compact_mirror_pending::MirrorPending;
use crate::compact_recovery_state::RecoveryCheckpoints;
use crate::compact_recovery_types::{RecoveryCaller, RecoveryStreamState};
use crate::jsonl::serialize_json_line;
use crate::socket_event_fanout::{QueueEntry, SocketEventQueueOverflowError, SocketEventSinkActor};
use futures::future::BoxFuture;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

pub type MirrorAdmissionsFn = Arc<dyn Fn(&str, &str) -> Vec<String> + Send + Sync>;
pub type EndOpenFn = Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>;

pub struct OpenedAdmission {
    pub admission_id: String,
    pub end: Option<EndOpenFn>,
}

pub struct AdmissionRoutingActor {
    sink: Option<SocketEventSinkActor>,
    peer: Option<String>,
    socket_epoch: Option<String>,
    checkpoints: Option<RecoveryCheckpoints>,
    pub admissions: HashSet<String>,
    pub subscriptions: HashMap<String, String>,
    pub mirror_owners: HashMap<String, String>,
    pub pending_mirrors: HashMap<String, Arc<Mutex<MirrorPending>>>,
    /// RULING-117: per session, the completed open that routes it on this socket and how that open ends.
    pub opened: HashMap<String, OpenedAdmission>,
    /// RULING-118: per request id, the route a detach retires once the request's reply is routed.
    retire_after_reply: HashMap<String, Box<dyn FnOnce() + Send>>,
    pub mirror_admissions: Option<MirrorAdmissionsFn>,
    terminal: HashMap<String, RecoveryStreamState>,
}

impl Default for AdmissionRoutingActor {
    fn default() -> Self {
        Self::new(None)
    }
}

impl AdmissionRoutingActor {
    pub fn new(sink: Option<SocketEventSinkActor>) -> Self {
        Self {
            sink,
            peer: None,
            socket_epoch: None,
            checkpoints: None,
            admissions: HashSet::new(),
            subscriptions: HashMap::new(),
            mirror_owners: HashMap::new(),
            pending_mirrors: HashMap::new(),
            opened: HashMap::new(),
            retire_after_reply: HashMap::new(),
            mirror_admissions: None,
            terminal: HashMap::new(),
        }
    }

    pub fn bind(
        &mut self,
        checkpoints: RecoveryCheckpoints,
        socket_epoch: impl Into<String>,
        peer: impl Into<String>,
    ) {
        self.checkpoints = Some(checkpoints);
        self.socket_epoch = Some(socket_epoch.into());
        self.peer = Some(peer.into());
    }

    pub fn close(&mut self) {
        self.subscriptions.clear();
        self.mirror_owners.clear();
        for pending in self.pending_mirrors.values() {
            MirrorPending::retire_shared(pending, None);
        }
        self.pending_mirrors.clear();
        self.admissions.clear();
        self.opened.clear();
        self.retire_after_reply.clear();
        self.terminal.clear();
        if let Some(sink) = &self.sink {
            sink.close();
        }
    }

    pub fn remember(
        &mut self,
        session_id: &str,
        checkpoints: &RecoveryCheckpoints,
        socket_epoch: &str,
    ) -> Option<RecoveryStreamState> {
        let stream = checkpoints.stream(&RecoveryCaller {
            socket_epoch: socket_epoch.to_owned(),
            session_id: session_id.to_owned(),
        });
        let remembered = stream
            .clone()
            .or_else(|| self.terminal.get(session_id).cloned())?;
        let mirror_values: HashSet<&str> =
            self.mirror_owners.values().map(String::as_str).collect();
        let routes: Vec<String> = remembered
            .routes
            .iter()
            .filter(|id| self.admissions.contains(*id) || mirror_values.contains(id.as_str()))
            .cloned()
            .collect();
        if let Some(s) = stream
            && !routes.is_empty()
        {
            self.terminal.insert(
                session_id.to_owned(),
                RecoveryStreamState {
                    session_epoch: s.session_epoch,
                    generation: s.generation,
                    routes: routes.clone(),
                },
            );
        }
        Some(RecoveryStreamState {
            session_epoch: remembered.session_epoch,
            generation: remembered.generation,
            routes,
        })
    }

    pub fn route(
        &mut self,
        line: &str,
        checkpoints: &RecoveryCheckpoints,
        socket_epoch: &str,
    ) -> Vec<String> {
        if self.admissions.is_empty()
            && self.subscriptions.is_empty()
            && self.mirror_owners.is_empty()
        {
            return vec![line.to_owned()];
        }
        let Ok(mut record) = serde_json::from_str::<Value>(line.trim_end()) else {
            return vec![line.to_owned()];
        };
        let Some(session_id) = record
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return vec![line.to_owned()];
        };
        if record.get("admissionId").is_some_and(|v| !v.is_null()) {
            return vec![line.to_owned()];
        }
        if let Some(sub_id) = record.get("subscriptionId").and_then(Value::as_str) {
            let Some(adm) = self.subscriptions.get(sub_id) else {
                return vec![line.to_owned()];
            };
            record["admissionId"] = Value::String(adm.clone());
            record["generation"] = serde_json::json!(0);
            return serialize_json_line(&record)
                .map(|s| vec![s])
                .unwrap_or_else(|_| vec![line.to_owned()]);
        }

        let record_type = record
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let mirrors = if record_type == "response" {
            Vec::new()
        } else if let Some(ref f) = self.mirror_admissions {
            f(&session_id, &record_type)
        } else {
            Vec::new()
        };

        let stream = self.remember(&session_id, checkpoints, socket_epoch);
        let terminal = record_type == "session_closed" || record_type == "session_parked";
        let mirror_values: HashSet<String> = self.mirror_owners.values().cloned().collect();
        let mut routes = Vec::new();
        let mut seen = HashSet::new();
        if let Some(ref st) = stream {
            for id in &st.routes {
                if (self.admissions.contains(id) || (terminal && mirror_values.contains(id)))
                    && seen.insert(id.clone())
                {
                    routes.push(id.clone());
                }
            }
        }
        for id in mirrors {
            if seen.insert(id.clone()) {
                routes.push(id);
            }
        }

        let generation = stream.as_ref().map(|s| s.generation);
        if record_type == "response"
            && let Some(req_id) = record.get("id").and_then(Value::as_str).map(str::to_owned)
        {
            return match reply_admission(&req_id, self.peer.as_deref(), &routes) {
                ReplyAdmission::Drop => Vec::new(),
                ReplyAdmission::Legacy => vec![line.to_owned()],
                ReplyAdmission::Route(adm) => {
                    record["admissionId"] = Value::String(adm);
                    if let Some(g) = generation {
                        record["generation"] = serde_json::json!(g);
                    }
                    serialize_json_line(&record)
                        .map(|s| vec![s])
                        .unwrap_or_else(|_| vec![line.to_owned()])
                }
            };
        }

        if routes.is_empty() {
            return vec![line.to_owned()];
        }
        routes
            .into_iter()
            .map(|adm| {
                let mut tagged = record.clone();
                tagged["admissionId"] = Value::String(adm);
                if let Some(g) = generation {
                    tagged["generation"] = serde_json::json!(g);
                }
                serialize_json_line(&tagged).unwrap_or_else(|_| line.to_owned())
            })
            .collect()
    }

    pub fn defer_retire(&mut self, id: impl Into<String>, retire: impl FnOnce() + Send + 'static) {
        self.retire_after_reply.insert(id.into(), Box::new(retire));
    }

    fn retire_replied(&mut self, line: &str) {
        if self.retire_after_reply.is_empty() {
            return;
        }
        let Ok(record) = serde_json::from_str::<Value>(line.trim_end()) else {
            return;
        };
        if record.get("type").and_then(Value::as_str) != Some("response") {
            return;
        }
        let Some(id) = record.get("id").and_then(Value::as_str) else {
            return;
        };
        if let Some(retire) = self.retire_after_reply.remove(id) {
            retire();
        }
    }

    pub fn enqueue_routed(
        &mut self,
        line: &str,
        key: Option<&str>,
        demoted: Option<&str>,
        written: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    ) -> Result<(), SocketEventQueueOverflowError> {
        let lines = match (&self.checkpoints, &self.socket_epoch) {
            (Some(cp), Some(se)) => {
                let cp = cp.clone();
                let se = se.clone();
                self.route(line, &cp, &se)
            }
            _ => vec![line.to_owned()],
        };
        self.retire_replied(line);
        if lines.is_empty() {
            if let Some(cb) = written {
                cb(false);
            }
            return Ok(());
        }
        let replacements = match (demoted, &self.checkpoints, &self.socket_epoch) {
            (Some(d), Some(cp), Some(se)) => {
                let cp = cp.clone();
                let se = se.clone();
                Some(self.route(d, &cp, &se))
            }
            _ => None,
        };

        let remaining = Arc::new(Mutex::new((lines.len(), false)));
        let overflow = Arc::new(Mutex::new(None));
        for (i, routed_line) in lines.into_iter().enumerate() {
            let adm_id = serde_json::from_str::<Value>(routed_line.trim_end())
                .ok()
                .and_then(|v| {
                    v.get("admissionId")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                });
            let pending = adm_id
                .as_deref()
                .and_then(|id| self.pending_mirrors.get(id))
                .cloned();
            let sink = self.sink.clone();
            let entry_key = key.map(|k| format!("{k}:{i}"));
            let demoted_line = replacements.as_ref().and_then(|r| r.get(i)).cloned();
            let rem_ok = remaining.clone();
            let written_ok = written.clone();
            let overflow_for_delivery = Arc::clone(&overflow);
            let line_for_deliver = routed_line.clone();
            let deliver = move || {
                let mut ok = true;
                if let Some(sink) = &sink
                    && let Err(error) = sink.enqueue(QueueEntry {
                        line: line_for_deliver,
                        key: entry_key,
                        demoted_line,
                        on_written: None,
                    })
                {
                    ok = false;
                    *overflow_for_delivery
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner) = Some(error);
                }
                let finished = {
                    let mut st = rem_ok.lock().unwrap_or_else(PoisonError::into_inner);
                    st.1 |= ok;
                    st.0 -= 1;
                    (st.0 == 0).then_some(st.1)
                };
                if let Some(success) = finished
                    && let Some(cb) = &written_ok
                {
                    cb(success);
                }
            };
            if let Some(pending) = pending {
                let rem_drop = remaining.clone();
                let written_drop = written.clone();
                let drop_fn: Box<dyn FnOnce() + Send> = Box::new(move || {
                    let finished = {
                        let mut st = rem_drop.lock().unwrap_or_else(PoisonError::into_inner);
                        st.0 -= 1;
                        (st.0 == 0).then_some(st.1)
                    };
                    if let Some(success) = finished
                        && let Some(cb) = &written_drop
                    {
                        cb(success);
                    }
                });
                MirrorPending::enqueue_shared(&pending, &routed_line, deliver, Some(drop_fn));
            } else {
                deliver();
            }
        }
        let error = overflow
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        error.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_admission_reply::{AdmissionReplyScope, with_admission_reply_sync};
    use crate::compact_content_store::CompactContentStoreOptions;
    use crate::compact_recovery_state::RecoveryRoute;
    use crate::compact_recovery_state::{FrozenLaneCut, RecoveryLane};
    use crate::compact_recovery_types::RecoveryRouteKind;
    use crate::compact_settled_messages::SettledRead;
    use crate::session_acceptance_lane::{FrozenAcceptedRoot, RetireReason, SessionAcceptanceLane};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

    #[tokio::test]
    async fn direct_routed_overflow_returns_queue_error() {
        let sink = SocketEventSinkActor::new(
            tokio::io::sink(),
            1,
            30_000,
            Arc::new(Mutex::new(
                crate::loop_blocked_time::LoopBlockedTime::default(),
            )),
            |_| {},
        );
        let mut actor = AdmissionRoutingActor::new(Some(sink));
        let result = actor.enqueue_routed("{\"type\":\"event\"}\n", None, None, None);
        assert!(result.is_err(), "queue overflow must reach the caller");
        actor.close();
    }

    #[test]
    fn active_mirror_written_callback_can_reenter_pending() {
        let pending = Arc::new(Mutex::new(MirrorPending::new()));
        pending.lock().expect("pending").activate();
        let mut actor = AdmissionRoutingActor::default();
        actor
            .pending_mirrors
            .insert("adm-1".into(), Arc::clone(&pending));
        let unlocked = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&unlocked);
        let pending_for_callback = Arc::clone(&pending);
        actor
            .enqueue_routed(
                "{\"type\":\"event\",\"admissionId\":\"adm-1\"}\n",
                None,
                None,
                Some(Arc::new(move |_| {
                    observed.store(pending_for_callback.try_lock().is_ok(), Ordering::SeqCst);
                })),
            )
            .expect("enqueue");
        assert!(
            unlocked.load(Ordering::SeqCst),
            "callback ran under mirror lock"
        );
        actor.close();
    }

    struct TestLane {
        lane: Mutex<SessionAcceptanceLane>,
        next_key: AtomicU64,
        roots: Mutex<HashMap<usize, FrozenAcceptedRoot>>,
    }

    impl TestLane {
        fn new(dir: &tempfile::TempDir) -> Self {
            let mut lane = SessionAcceptanceLane::new(
                Some(CompactContentStoreOptions {
                    dir: dir.path().join("s").display().to_string(),
                    ..Default::default()
                }),
                None,
            );
            lane.bootstrap(Default::default(), Some("n1".into()));
            Self {
                lane: Mutex::new(lane),
                next_key: AtomicU64::new(1),
                roots: Mutex::new(HashMap::new()),
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
            Some(3)
        }
        fn freeze_cut(&self) -> Option<FrozenLaneCut> {
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
            Box::pin(async { Ok(json!([])) })
        }
        fn read_calls(
            &self,
            _: usize,
        ) -> BoxFuture<'static, Result<Value, crate::compact_recovery_bytes::RecoveryBytesError>>
        {
            Box::pin(async { Ok(json!({"calls":[]})) })
        }
    }

    #[test]
    fn routes_events_subscriptions_and_scoped_replies_with_deferred_retire() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lane = Arc::new(TestLane::new(&dir));
        let checkpoints = RecoveryCheckpoints::default();
        checkpoints
            .freeze(
                &RecoveryRoute {
                    socket_epoch: "se-1".into(),
                    admission_id: "adm-1".into(),
                    route_kind: RecoveryRouteKind::Ordinary,
                    peer: "p1".into(),
                    session_id: "s1".into(),
                },
                lane,
                false,
            )
            .expect("freeze");

        let mut actor = AdmissionRoutingActor::default();
        actor.bind(checkpoints.clone(), "se-1", "p1");
        actor.admissions.insert("adm-1".into());
        actor.subscriptions.insert("sub-1".into(), "sub-adm".into());

        // Subscription record
        let sub_line = format!(
            "{}\n",
            json!({"type":"session_status","sessionId":"s1","subscriptionId":"sub-1"})
        );
        let routed_sub = actor.route(&sub_line, &checkpoints, "se-1");
        let parsed_sub: Value = serde_json::from_str(&routed_sub[0]).expect("json");
        assert_eq!(parsed_sub["admissionId"], "sub-adm");
        assert_eq!(parsed_sub["generation"], 0);

        // Session event
        let ev_line = format!("{}\n", json!({"type":"turn_start","sessionId":"s1"}));
        let routed_ev = actor.route(&ev_line, &checkpoints, "se-1");
        let parsed_ev: Value = serde_json::from_str(&routed_ev[0]).expect("json");
        assert_eq!(parsed_ev["admissionId"], "adm-1");
        assert_eq!(parsed_ev["generation"], 3);

        // Response under admission reply scope
        let resp_line = format!(
            "{}\n",
            json!({"type":"response","id":"r1","sessionId":"s1","success":true})
        );
        with_admission_reply_sync(
            Some(AdmissionReplyScope {
                id: Some("r1".into()),
                admission_id: Some("adm-1".into()),
                peer: Some("p1".into()),
            }),
            || {
                let routed_resp = actor.route(&resp_line, &checkpoints, "se-1");
                assert_eq!(routed_resp.len(), 1);
                let parsed: Value = serde_json::from_str(&routed_resp[0]).expect("json");
                assert_eq!(parsed["admissionId"], "adm-1");
            },
        );

        let retired = Arc::new(AtomicBool::new(false));
        let r_flag = retired.clone();
        actor.defer_retire("r1", move || r_flag.store(true, Ordering::SeqCst));
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        actor
            .enqueue_routed(
                &resp_line,
                None,
                None,
                Some(Arc::new(move |_| {
                    c.fetch_add(1, Ordering::SeqCst);
                })),
            )
            .expect("enqueue");
        assert!(retired.load(Ordering::SeqCst));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
