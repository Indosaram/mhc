//! senpi `modes/rpc/compact-open-admission.ts`: registers the client's admission ID before
//! physical open/attach so identical pending/completed retries never open again.

use crate::compact_admission_baseline::{
    BaselineIdentity, GeneratedFramesCursor, admission_baseline,
};
use crate::compact_admission_error::{CompactAdmissionError, CompactAdmissionReason};
use crate::compact_admission_producer::ProducerAbortSignal;
use crate::compact_admission_storage::AdmissionState;
use crate::compact_admission_types::{AdmissionIdentity, Answer, Produced};
use crate::compact_admissions::CompactAdmissions;
use crate::compact_recovery_state::{
    RecoveryCheckpoints, RecoveryLane, RecoveryRoute, RouteRetirement,
};
use crate::compact_recovery_types::{RecoveryCaller, RecoveryRouteKind};
use futures::future::BoxFuture;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedSessionInfo {
    pub session_id: String,
    pub attached: bool,
}

pub type OpenAcceptance = Arc<
    dyn Fn(OpenedSessionInfo) -> BoxFuture<'static, Result<(), CompactAdmissionError>>
        + Send
        + Sync,
>;

type OpenReleaseAction = Box<dyn FnOnce() -> BoxFuture<'static, Result<(), String>> + Send>;
type OpenBaselineFn = Box<dyn FnOnce() + Send>;
type CompletedOpenRelease = Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>;
type CompletedOpenSlot = Arc<Mutex<Option<(String, CompletedOpenRelease)>>>;
type RetireAfterReplySlot = Arc<Mutex<Option<Box<dyn FnOnce(RouteRetirement) + Send>>>>;
type OpenResultSlot = Arc<Mutex<Option<(Answer, CompletedOpenRelease)>>>;

pub trait OpenRouterHost: Send + Sync {
    fn current_connection(&self) -> Option<String>;
    fn socket_epoch_of(&self, peer: &str) -> Option<String>;
    fn admissions(&self) -> Option<Arc<CompactAdmissions>>;
    fn recovery_lane(&self, session_id: &str) -> Option<Arc<dyn RecoveryLane>>;
    fn recovery_checkpoints(&self) -> RecoveryCheckpoints;
    fn register_admission(&self, peer: &str, admission_id: &str, session_id: &str);
    fn unregister_admission(&self, peer: &str, admission_id: &str);
    fn install_admission_baseline(
        &self,
        peer: &str,
        baseline: GeneratedFramesCursor,
        still_same_epoch: Box<dyn Fn() -> bool + Send + Sync>,
    );
    fn supersede_open(
        &self,
        peer: &str,
        session_id: &str,
        admission_id: &str,
        release: Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>,
    ) -> Option<OpenReleaseAction>;
    fn release_owner_attachment(&self, peer: &str, session_id: &str);
    fn release_disconnect_attachment(&self, session_id: &str) -> BoxFuture<'static, ()>;
    fn enqueue_control(
        &self,
        response: Value,
        peer: &str,
    ) -> BoxFuture<'static, Result<(), String>>;
}

pub struct OpenCommandInput {
    pub id: Option<String>,
    pub command_type: String,
    pub admission_id: Option<String>,
    pub retry: bool,
    /// All remaining request fields used for immutable open-identity fingerprinting.
    pub request_fields: BTreeMap<String, Value>,
}

fn error_response(id: Option<&str>, command: &str, code: &str, data: Option<Value>) -> Value {
    let mut resp = json!({
        "type": "response",
        "command": command,
        "success": false,
        "error": code,
        "errorCode": code,
    });
    if let Some(id) = id {
        resp["id"] = Value::String(id.to_owned());
    }
    if let Some(data) = data {
        resp["errorData"] = data;
    }
    resp
}

fn fingerprint_request(fields: &BTreeMap<String, Value>) -> String {
    let entries: Vec<(&String, &Value)> = fields.iter().collect();
    let serialized = serde_json::to_string(&entries).unwrap_or_else(|_| "[]".into());
    hex::encode(Sha256::digest(serialized.as_bytes()))
}

pub async fn compact_open(
    host: Arc<dyn OpenRouterHost>,
    command: OpenCommandInput,
    physical: impl FnOnce(
        OpenAcceptance,
        ProducerAbortSignal,
    ) -> BoxFuture<'static, Result<Option<Value>, Value>>
    + Send
    + 'static,
) -> Result<Option<Value>, Value> {
    let peer = host.current_connection();
    let socket_epoch = peer.as_deref().and_then(|p| host.socket_epoch_of(p));
    let admissions = host.admissions();
    let (Some(peer), Some(socket_epoch), Some(admissions)) = (peer, socket_epoch, admissions)
    else {
        return Ok(Some(error_response(
            command.id.as_deref(),
            &command.command_type,
            "unsupported_transport",
            None,
        )));
    };
    let Some(admission_id) = command.admission_id.clone() else {
        return Ok(Some(error_response(
            command.id.as_deref(),
            &command.command_type,
            "admission_malformed_request",
            Some(
                json!({"kind":"request_refused","reason":"malformed_request","hint":"new_request"}),
            ),
        )));
    };

    let fingerprint = fingerprint_request(&command.request_fields);
    let delivery_tx: Arc<Mutex<Option<tokio::sync::oneshot::Sender<Option<Answer>>>>> =
        Arc::new(Mutex::new(None));
    let baseline_fn: Arc<Mutex<Option<OpenBaselineFn>>> = Arc::new(Mutex::new(None));
    let completed_open: CompletedOpenSlot = Arc::new(Mutex::new(None));
    let accepted: Arc<Mutex<Option<(String, u64)>>> = Arc::new(Mutex::new(None));
    let retire_after_reply: RetireAfterReplySlot = Arc::new(Mutex::new(None));
    let announced: Arc<Mutex<Option<RouteRetirement>>> = Arc::new(Mutex::new(None));

    let answer_finally = {
        let announced = announced.clone();
        let retire_after_reply = retire_after_reply.clone();
        move |emit: Box<dyn FnOnce() + Send>| {
            emit();
            let retirement = RouteRetirement::OpenEndedAnnounced;
            *announced.lock().unwrap_or_else(PoisonError::into_inner) = Some(retirement.clone());
            if let Some(cb) = retire_after_reply
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                cb(retirement);
            }
        }
    };

    let cancelled_answer = {
        let admissions = admissions.clone();
        let socket_epoch = socket_epoch.clone();
        let admission_id = admission_id.clone();
        let cmd_id = command.id.clone();
        let cmd_type = command.command_type.clone();
        move || {
            let admissions = admissions.clone();
            let socket_epoch = socket_epoch.clone();
            let admission_id = admission_id.clone();
            let cmd_id = cmd_id.clone();
            let cmd_type = cmd_type.clone();
            async move {
                let res = admissions
                    .result(&socket_epoch, &admission_id)
                    .await
                    .ok()??;
                if res.state != AdmissionState::Cancelled {
                    return None;
                }
                let generation = res.answer.get("generation").and_then(Value::as_u64)?;
                let mut map = Map::new();
                if let Some(id) = cmd_id {
                    map.insert("id".into(), Value::String(id));
                }
                map.insert("type".into(), json!("response"));
                map.insert("command".into(), Value::String(cmd_type));
                map.insert("success".into(), json!(false));
                map.insert("error".into(), json!("attempt_cancelled"));
                map.insert("errorCode".into(), json!("attempt_cancelled"));
                map.insert("code".into(), json!("attempt_cancelled"));
                map.insert("admissionId".into(), Value::String(admission_id));
                map.insert("generation".into(), json!(generation));
                Some(map)
            }
        }
    };

    let identity = AdmissionIdentity {
        socket_epoch: socket_epoch.clone(),
        peer: peer.clone(),
        admission_id: admission_id.clone(),
        fingerprint,
        retry: command.retry,
    };

    enum PhysicalOrAdmissionError {
        OpenRefused(Value),
        Admission(CompactAdmissionError),
        Unexpected(Value),
    }

    let physical_refusal: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let unexpected_err: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));

    let run_res = {
        let host = host.clone();
        let peer = peer.clone();
        let socket_epoch = socket_epoch.clone();
        let admission_id = admission_id.clone();
        let cmd_type = command.command_type.clone();
        let delivery_tx = delivery_tx.clone();
        let baseline_fn = baseline_fn.clone();
        let completed_open = completed_open.clone();
        let accepted = accepted.clone();
        let retire_after_reply = retire_after_reply.clone();
        let announced = announced.clone();
        let physical_refusal = physical_refusal.clone();
        let unexpected_err = unexpected_err.clone();

        admissions
            .run(identity, move |signal| {
                Box::pin(async move {
                    let result_slot: OpenResultSlot = Arc::new(Mutex::new(None));
                    let physical_succeeded = Arc::new(AtomicBool::new(false));

                    let accept: OpenAcceptance = {
                        let host = host.clone();
                        let peer = peer.clone();
                        let socket_epoch = socket_epoch.clone();
                        let admission_id = admission_id.clone();
                        let cmd_type = cmd_type.clone();
                        let delivery_tx = delivery_tx.clone();
                        let baseline_fn = baseline_fn.clone();
                        let completed_open = completed_open.clone();
                        let accepted = accepted.clone();
                        let retire_after_reply = retire_after_reply.clone();
                        let announced = announced.clone();
                        let result_slot = result_slot.clone();
                        let physical_succeeded = physical_succeeded.clone();

                        Arc::new(move |opened: OpenedSessionInfo| {
                            let host = host.clone();
                            let peer = peer.clone();
                            let socket_epoch = socket_epoch.clone();
                            let admission_id = admission_id.clone();
                            let cmd_type = cmd_type.clone();
                            let delivery_tx = delivery_tx.clone();
                            let baseline_fn = baseline_fn.clone();
                            let completed_open = completed_open.clone();
                            let accepted = accepted.clone();
                            let retire_after_reply = retire_after_reply.clone();
                            let announced = announced.clone();
                            let result_slot = result_slot.clone();
                            let physical_succeeded = physical_succeeded.clone();

                            Box::pin(async move {
                                let session_id = opened.session_id;
                                let Some(lane) = host.recovery_lane(&session_id) else {
                                    return Err(CompactAdmissionError::new(
                                        CompactAdmissionReason::StorageUnavailable,
                                    ));
                                };
                                let checkpoints = host.recovery_checkpoints();
                                let route = RecoveryRoute {
                                    socket_epoch: socket_epoch.clone(),
                                    admission_id: admission_id.clone(),
                                    route_kind: RecoveryRouteKind::Ordinary,
                                    peer: peer.clone(),
                                    session_id: session_id.clone(),
                                };
                                let frozen =
                                    checkpoints.freeze(&route, lane, true).map_err(|_| {
                                        CompactAdmissionError::new(
                                            CompactAdmissionReason::StorageUnavailable,
                                        )
                                    })?;
                                let token = frozen.token.clone();
                                let generation = token.admission.generation;
                                let active = Arc::new(AtomicBool::new(true));
                                let (tx, rx) = tokio::sync::oneshot::channel::<Option<Answer>>();
                                *delivery_tx.lock().unwrap_or_else(PoisonError::into_inner) =
                                    Some(tx);

                                host.register_admission(&peer, &admission_id, &session_id);
                                let baseline_host = host.clone();
                                let baseline_peer = peer.clone();
                                let baseline_se = socket_epoch.clone();
                                let baseline_ident = BaselineIdentity {
                                    session_id: session_id.clone(),
                                    admission_id: admission_id.clone(),
                                    generation,
                                    token: Some(token.id.clone()),
                                    event_lease: None,
                                    component_ref: None,
                                };
                                let active_for_baseline = active.clone();
                                *baseline_fn.lock().unwrap_or_else(PoisonError::into_inner) =
                                    Some(Box::new(move || {
                                        let cursor = admission_baseline(
                                            Box::pin(async move { rx.await.ok().flatten() }),
                                            baseline_ident,
                                            Arc::new(move || {
                                                active_for_baseline.load(Ordering::SeqCst)
                                            }),
                                            None,
                                        );
                                        let se_check_host = baseline_host.clone();
                                        let se_check_peer = baseline_peer.clone();
                                        baseline_host.install_admission_baseline(
                                            &baseline_peer,
                                            cursor,
                                            Box::new(move || {
                                                se_check_host
                                                    .socket_epoch_of(&se_check_peer)
                                                    .as_deref()
                                                    == Some(baseline_se.as_str())
                                            }),
                                        );
                                    }));

                                *accepted.lock().unwrap_or_else(PoisonError::into_inner) =
                                    Some((admission_id.clone(), generation));

                                let make_release = |superseded: bool| -> Box<
                                    dyn FnOnce() -> BoxFuture<'static, ()> + Send,
                                > {
                                    let active = active.clone();
                                    let host = host.clone();
                                    let checkpoints = checkpoints.clone();
                                    let peer = peer.clone();
                                    let socket_epoch = socket_epoch.clone();
                                    let session_id = session_id.clone();
                                    let admission_id = admission_id.clone();
                                    let announced = announced.clone();
                                    let retire_after_reply = retire_after_reply.clone();
                                    let physical_succeeded = physical_succeeded.clone();
                                    Box::new(move || {
                                        Box::pin(async move {
                                            if !active.swap(false, Ordering::SeqCst) {
                                                return;
                                            }
                                            let retire = {
                                                let host = host.clone();
                                                let checkpoints = checkpoints.clone();
                                                let peer = peer.clone();
                                                let caller = RecoveryCaller {
                                                    socket_epoch,
                                                    session_id: session_id.clone(),
                                                };
                                                let adm = admission_id.clone();
                                                Box::new(move |retirement: RouteRetirement| {
                                                    checkpoints
                                                        .retire_route(&caller, &adm, retirement);
                                                    host.unregister_admission(&peer, &adm);
                                                })
                                            };
                                            if superseded {
                                                retire(RouteRetirement::ReopenSuperseded);
                                            } else if let Some(ann) = announced
                                                .lock()
                                                .unwrap_or_else(PoisonError::into_inner)
                                                .clone()
                                            {
                                                retire(ann);
                                            } else {
                                                *retire_after_reply
                                                    .lock()
                                                    .unwrap_or_else(PoisonError::into_inner) =
                                                    Some(retire);
                                            }
                                            if physical_succeeded.load(Ordering::SeqCst) {
                                                host.release_owner_attachment(&peer, &session_id);
                                                host.release_disconnect_attachment(&session_id)
                                                    .await;
                                            }
                                        })
                                    })
                                };

                                *completed_open
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner) =
                                    Some((session_id.clone(), make_release(true)));

                                let mut data = Map::new();
                                data.insert("sessionId".into(), Value::String(session_id.clone()));
                                if opened.attached {
                                    data.insert("attached".into(), json!(true));
                                }
                                data.insert(
                                    "recovery".into(),
                                    json!({"token": token.id, "componentRef": "manifest"}),
                                );
                                let mut ans = Map::new();
                                ans.insert("type".into(), json!("response"));
                                ans.insert("command".into(), Value::String(cmd_type));
                                ans.insert("success".into(), json!(true));
                                ans.insert("sessionId".into(), Value::String(session_id.clone()));
                                ans.insert(
                                    "admissionId".into(),
                                    Value::String(admission_id.clone()),
                                );
                                ans.insert("generation".into(), json!(generation));
                                ans.insert("data".into(), Value::Object(data));

                                *result_slot.lock().unwrap_or_else(PoisonError::into_inner) =
                                    Some((ans, make_release(false)));
                                Ok(())
                            })
                        })
                    };

                    let phys_out = physical(accept, signal).await;
                    let slot = result_slot
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .take();
                    match phys_out {
                        Err(err_val) => {
                            if let Some((_, rel)) = slot {
                                rel().await;
                            }
                            *unexpected_err
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner) = Some(err_val);
                            Err(CompactAdmissionError::new(
                                CompactAdmissionReason::StorageUnavailable,
                            ))
                        }
                        Ok(Some(resp)) => {
                            if let Some((_, rel)) = slot {
                                rel().await;
                            }
                            *physical_refusal
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner) = Some(resp);
                            Err(CompactAdmissionError::new(
                                CompactAdmissionReason::StorageUnavailable,
                            ))
                        }
                        Ok(None) => {
                            let Some((answer, release)) = slot else {
                                return Err(CompactAdmissionError::new(
                                    CompactAdmissionReason::StorageUnavailable,
                                ));
                            };
                            physical_succeeded.store(true, Ordering::SeqCst);
                            Ok(Produced {
                                answer,
                                release: Some(Box::new(move || {
                                    Box::pin(async move {
                                        release().await;
                                        Ok(())
                                    })
                                })),
                                activate: None,
                                refusal: None,
                            })
                        }
                    }
                })
            })
            .await
    };

    match run_res {
        Ok(answer) => {
            let cancelled = cancelled_answer().await;
            let mut response = cancelled.clone().unwrap_or(answer);
            if let Some(id) = command.id {
                response.insert("id".into(), Value::String(id));
            } else {
                response.remove("id");
            }
            let tx = delivery_tx
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            if let Some(tx) = tx {
                let is_ok = response.get("success").and_then(Value::as_bool) == Some(true);
                let comp = completed_open
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                if cancelled.is_none()
                    && is_ok
                    && let Some((sess_id, rel)) = comp
                    && let Some(sup) = host.supersede_open(&peer, &sess_id, &admission_id, rel)
                {
                    tokio::spawn(async move {
                        if let Err(e) = sup().await {
                            eprintln!("senpi rpc superseded open release failed: {e}");
                        }
                    });
                }
                let bl = baseline_fn
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                let deliver: Box<dyn FnOnce() + Send> = Box::new(move || {
                    if let Some(bl) = bl {
                        bl();
                    }
                    let _ = tx.send(Some(response));
                });
                if cancelled.is_some() {
                    answer_finally(deliver);
                } else {
                    deliver();
                }
            } else {
                let _ = host.enqueue_control(Value::Object(response), &peer).await;
            }
            Ok(None)
        }
        Err(cause) => {
            if let Some(tx) = delivery_tx
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                let _ = tx.send(None);
            }
            let err_kind = if let Some(resp) = physical_refusal
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                PhysicalOrAdmissionError::OpenRefused(resp)
            } else if let Some(unexp) = unexpected_err
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                PhysicalOrAdmissionError::Unexpected(unexp)
            } else {
                PhysicalOrAdmissionError::Admission(cause)
            };

            let acc = accepted
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            match err_kind {
                PhysicalOrAdmissionError::Unexpected(mut unexp) => {
                    if let Some((adm_id, gen_)) = acc {
                        if let Some(obj) = unexp.as_object_mut() {
                            obj.insert("admissionId".into(), Value::String(adm_id));
                            obj.insert("generation".into(), json!(gen_));
                        }
                        let h = host.clone();
                        let p = peer.clone();
                        let u = unexp.clone();
                        answer_finally(Box::new(move || {
                            tokio::spawn(async move {
                                if let Err(e) = h.enqueue_control(u, &p).await {
                                    eprintln!("senpi rpc failed open's refusal not written: {e}");
                                }
                            });
                        }));
                    }
                    Err(unexp)
                }
                PhysicalOrAdmissionError::OpenRefused(resp) => {
                    let Some((adm_id, gen_)) = acc else {
                        return Ok(Some(resp));
                    };
                    let mut ended = resp;
                    if let Some(obj) = ended.as_object_mut() {
                        obj.insert("admissionId".into(), Value::String(adm_id));
                        obj.insert("generation".into(), json!(gen_));
                    }
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let h = host.clone();
                    let p = peer.clone();
                    answer_finally(Box::new(move || {
                        let fut = h.enqueue_control(ended, &p);
                        let _ = tx.send(fut);
                    }));
                    if let Ok(fut) = rx.await {
                        let _ = fut.await;
                    }
                    Ok(None)
                }
                PhysicalOrAdmissionError::Admission(adm_err) => {
                    let refusal = match cancelled_answer().await {
                        Some(c) => Value::Object(c),
                        None => error_response(
                            command.id.as_deref(),
                            &command.command_type,
                            &format!("admission_{}", adm_err.reason.as_str()),
                            Some(json!({
                                "kind": "request_refused",
                                "reason": adm_err.reason.as_str(),
                                "hint": "new_request",
                            })),
                        ),
                    };
                    let Some((adm_id, gen_)) = acc else {
                        return Ok(Some(refusal));
                    };
                    let mut ended = refusal;
                    if let Some(obj) = ended.as_object_mut() {
                        obj.insert("admissionId".into(), Value::String(adm_id));
                        obj.insert("generation".into(), json!(gen_));
                    }
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let h = host.clone();
                    let p = peer.clone();
                    answer_finally(Box::new(move || {
                        let fut = h.enqueue_control(ended, &p);
                        let _ = tx.send(fut);
                    }));
                    if let Ok(fut) = rx.await {
                        let _ = fut.await;
                    }
                    Ok(None)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_admission_baseline::LegacyViewCursor;
    use crate::compact_recovery_state::FrozenLaneCut;
    use crate::compact_settled_messages::SettledRead;
    use crate::session_acceptance_lane::{FrozenAcceptedRoot, RetireReason, SessionAcceptanceLane};
    use std::collections::HashMap;
    use std::sync::atomic::AtomicU64;

    struct TestLane {
        lane: Mutex<SessionAcceptanceLane>,
        next_key: AtomicU64,
        roots: Mutex<HashMap<usize, FrozenAcceptedRoot>>,
    }

    impl TestLane {
        fn new() -> Self {
            let mut lane = SessionAcceptanceLane::default();
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
            0
        }
        fn retired(&self) -> bool {
            false
        }
        fn retire_reason(&self) -> Option<RetireReason> {
            None
        }
        fn next_generation(&self) -> Option<u64> {
            Some(1)
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

    struct MockOpenHost {
        admissions: Arc<CompactAdmissions>,
        checkpoints: RecoveryCheckpoints,
        lane: Arc<TestLane>,
        baselines: Mutex<Vec<GeneratedFramesCursor>>,
        controls: Mutex<Vec<Value>>,
    }

    impl OpenRouterHost for MockOpenHost {
        fn current_connection(&self) -> Option<String> {
            Some("p1".into())
        }
        fn socket_epoch_of(&self, _: &str) -> Option<String> {
            Some("se-1".into())
        }
        fn admissions(&self) -> Option<Arc<CompactAdmissions>> {
            Some(self.admissions.clone())
        }
        fn recovery_lane(&self, _: &str) -> Option<Arc<dyn RecoveryLane>> {
            Some(self.lane.clone())
        }
        fn recovery_checkpoints(&self) -> RecoveryCheckpoints {
            self.checkpoints.clone()
        }
        fn register_admission(&self, _: &str, _: &str, _: &str) {}
        fn unregister_admission(&self, _: &str, _: &str) {}
        fn install_admission_baseline(
            &self,
            _: &str,
            baseline: GeneratedFramesCursor,
            _: Box<dyn Fn() -> bool + Send + Sync>,
        ) {
            self.baselines
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(baseline);
        }
        fn supersede_open(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>,
        ) -> Option<Box<dyn FnOnce() -> BoxFuture<'static, Result<(), String>> + Send>> {
            None
        }
        fn release_owner_attachment(&self, _: &str, _: &str) {}
        fn release_disconnect_attachment(&self, _: &str) -> BoxFuture<'static, ()> {
            Box::pin(async {})
        }
        fn enqueue_control(
            &self,
            response: Value,
            _: &str,
        ) -> BoxFuture<'static, Result<(), String>> {
            self.controls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(response);
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn opens_once_installs_baseline_and_replays_via_control_on_retry() {
        let host = Arc::new(MockOpenHost {
            admissions: Arc::new(CompactAdmissions::default()),
            checkpoints: RecoveryCheckpoints::default(),
            lane: Arc::new(TestLane::new()),
            baselines: Mutex::new(Vec::new()),
            controls: Mutex::new(Vec::new()),
        });
        let phys_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let c1 = phys_calls.clone();
        let res1 = compact_open(
            host.clone(),
            OpenCommandInput {
                id: Some("r1".into()),
                command_type: "open_session".into(),
                admission_id: Some("adm-1".into()),
                retry: false,
                request_fields: BTreeMap::from([("cwd".into(), json!("/tmp"))]),
            },
            move |accept, _| {
                Box::pin(async move {
                    c1.fetch_add(1, Ordering::SeqCst);
                    accept(OpenedSessionInfo {
                        session_id: "s1".into(),
                        attached: false,
                    })
                    .await
                    .map_err(|e| json!({"error": e.to_string()}))?;
                    Ok(None)
                })
            },
        )
        .await
        .expect("open 1");
        assert!(res1.is_none());
        assert_eq!(phys_calls.load(Ordering::SeqCst), 1);

        let mut bl = host
            .baselines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(0);
        let first_line = bl.next_frame().await.expect("reply frame");
        let second_line = bl.next_frame().await.expect("manifest frame");
        assert!(first_line.contains("\"sessionId\":\"s1\""));
        assert!(second_line.contains("\"type\":\"recovery_manifest\""));

        // Retry replays cached answer via enqueue_control without re-running physical open
        let c2 = phys_calls.clone();
        let res2 = compact_open(
            host.clone(),
            OpenCommandInput {
                id: Some("r2".into()),
                command_type: "open_session".into(),
                admission_id: Some("adm-1".into()),
                retry: true,
                request_fields: BTreeMap::from([("cwd".into(), json!("/tmp"))]),
            },
            move |_, _| {
                Box::pin(async move {
                    c2.fetch_add(1, Ordering::SeqCst);
                    Ok(None)
                })
            },
        )
        .await
        .expect("retry");
        assert!(res2.is_none());
        assert_eq!(phys_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            host.controls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len(),
            1
        );
        host.admissions.dispose().await;
    }
}
