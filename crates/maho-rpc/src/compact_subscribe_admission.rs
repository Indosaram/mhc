//! senpi `modes/rpc/compact-subscribe-admission.ts`: compact admission wrapper for
//! `subscribe_sessions` (bounded attempt index, seq-0 EventLease snapshot, and pending buffer).

use crate::compact_admission_baseline::{
    BaselineIdentity, GeneratedFramesCursor, admission_baseline,
};
use crate::compact_admission_error::{CompactAdmissionError, CompactAdmissionReason};
use crate::compact_admission_storage::AdmissionState;
use crate::compact_admission_types::{AdmissionIdentity, Answer, Produced};
use crate::compact_admissions::CompactAdmissions;
use crate::compact_event_leases::EventLeases;
use crate::compact_mirror_pending::{MirrorPending, MirrorPendingReason};
use crate::compact_recovery_types::EventLease;
use crate::compact_status_snapshot::status_snapshot;
use futures::future::BoxFuture;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

pub struct SubscribeCommandResult {
    pub answer: Value,
    pub activate_subscription_id: Option<String>,
    pub snapshot_rows: Vec<Value>,
}

pub trait SubscribeRouterHost: Send + Sync {
    fn current_connection(&self) -> Option<String>;
    fn socket_epoch_of(&self, peer: &str) -> Option<String>;
    fn recovery_events(&self) -> Option<EventLeases>;
    fn admissions(&self) -> Option<Arc<CompactAdmissions>>;
    fn answer_subscribe_sessions(&self, command: &Value, peer: &str) -> SubscribeCommandResult;
    fn register_subscription(&self, peer: &str, admission_id: &str, subscription_id: &str);
    fn unregister_subscription(&self, peer: &str, subscription_id: &str);
    fn pending_buffer(&self, subscription_id: &str) -> Option<Arc<Mutex<MirrorPending>>>;
    fn activate_status(&self, subscription_id: &str);
    fn unsubscribe_status(&self, peer: &str, subscription_id: &str);
    fn install_admission_baseline(
        &self,
        peer: &str,
        baseline: GeneratedFramesCursor,
        still_same_epoch: Box<dyn Fn() -> bool + Send + Sync>,
    );
    fn enqueue_control(
        &self,
        response: Value,
        peer: &str,
    ) -> BoxFuture<'static, Result<(), String>>;
}

pub struct SubscribeCommandInput {
    pub id: Option<String>,
    pub admission_id: Option<String>,
    pub retry: bool,
    pub raw_command: Value,
    pub fingerprint_fields: BTreeMap<String, Value>,
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

pub async fn compact_subscribe(
    host: Arc<dyn SubscribeRouterHost>,
    command: SubscribeCommandInput,
) -> Result<Option<Value>, CompactAdmissionError> {
    let peer = host.current_connection();
    let socket_epoch = peer.as_deref().and_then(|p| host.socket_epoch_of(p));
    let events = host.recovery_events();
    let admissions = host.admissions();
    let (Some(peer), Some(socket_epoch), Some(events), Some(admissions), Some(admission_id)) = (
        peer,
        socket_epoch,
        events,
        admissions,
        command.admission_id.clone(),
    ) else {
        return Ok(Some(error_response(
            command.id.as_deref(),
            "subscribe_sessions",
            "unsupported_transport",
            None,
        )));
    };

    let fingerprint = fingerprint_request(&command.fingerprint_fields);
    type BaselineAction = Box<dyn FnOnce() + Send>;
    let delivery_tx: Arc<Mutex<Option<tokio::sync::oneshot::Sender<Option<Answer>>>>> =
        Arc::new(Mutex::new(None));
    let baseline_fn: Arc<Mutex<Option<BaselineAction>>> = Arc::new(Mutex::new(None));

    let cancelled_answer = {
        let admissions = admissions.clone();
        let socket_epoch = socket_epoch.clone();
        let admission_id = admission_id.clone();
        let cmd_id = command.id.clone();
        move || {
            let admissions = admissions.clone();
            let socket_epoch = socket_epoch.clone();
            let admission_id = admission_id.clone();
            let cmd_id = cmd_id.clone();
            async move {
                let res = admissions
                    .result(&socket_epoch, &admission_id)
                    .await
                    .ok()??;
                if res.state != AdmissionState::Cancelled {
                    return None;
                }
                let mut map = Map::new();
                if let Some(id) = cmd_id {
                    map.insert("id".into(), Value::String(id));
                }
                map.insert("type".into(), json!("response"));
                map.insert("command".into(), json!("subscribe_sessions"));
                map.insert("success".into(), json!(false));
                map.insert("code".into(), json!("attempt_cancelled"));
                map.insert("errorCode".into(), json!("attempt_cancelled"));
                map.insert("error".into(), json!("attempt_cancelled"));
                map.insert("admissionId".into(), Value::String(admission_id));
                map.insert("generation".into(), json!(0));
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

    let run_res = {
        let host = host.clone();
        let events = events.clone();
        let peer = peer.clone();
        let socket_epoch = socket_epoch.clone();
        let admission_id = admission_id.clone();
        let raw_command = command.raw_command.clone();
        let delivery_tx = delivery_tx.clone();
        let baseline_fn = baseline_fn.clone();

        admissions
            .run(identity, move |_| {
                Box::pin(async move {
                    let subscribed = host.answer_subscribe_sessions(&raw_command, &peer);
                    let Some(subscription_id) = subscribed.activate_subscription_id else {
                        let map = subscribed.answer.as_object().cloned().unwrap_or_default();
                        return Ok(Produced::simple(map));
                    };
                    host.register_subscription(&peer, &admission_id, &subscription_id);
                    let lease: EventLease = status_snapshot(
                        &events,
                        &socket_epoch,
                        &admission_id,
                        &subscription_id,
                        &subscribed.snapshot_rows,
                    )?;
                    let Some(pending) = host.pending_buffer(&subscription_id) else {
                        return Err(CompactAdmissionError::new(
                            CompactAdmissionReason::StorageUnavailable,
                        ));
                    };

                    let (tx, rx) = tokio::sync::oneshot::channel::<Option<Answer>>();
                    *delivery_tx.lock().unwrap_or_else(PoisonError::into_inner) = Some(tx);
                    let live = Arc::new(AtomicBool::new(true));
                    let installed = Arc::new(AtomicBool::new(false));

                    let baseline_host = host.clone();
                    let baseline_peer = peer.clone();
                    let baseline_se = socket_epoch.clone();
                    let baseline_ident = BaselineIdentity {
                        session_id: subscription_id.clone(),
                        admission_id: admission_id.clone(),
                        generation: 0,
                        token: None,
                        event_lease: Some(lease.clone()),
                        component_ref: Some("record".into()),
                    };
                    let live_flag = live.clone();
                    let installed_flag = installed.clone();
                    let rx_slot = Arc::new(Mutex::new(Some(rx)));

                    let install_once: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                        if installed_flag.swap(true, Ordering::SeqCst) {
                            return;
                        }
                        let rx = rx_slot
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .take();
                        let live_inner = live_flag.clone();
                        let cursor = admission_baseline(
                            Box::pin(async move {
                                match rx {
                                    Some(rx) => rx.await.ok().flatten(),
                                    None => None,
                                }
                            }),
                            baseline_ident.clone(),
                            Arc::new(move || live_inner.load(Ordering::SeqCst)),
                            None,
                        );
                        let se_host = baseline_host.clone();
                        let se_peer = baseline_peer.clone();
                        let se_val = baseline_se.clone();
                        baseline_host.install_admission_baseline(
                            &baseline_peer,
                            cursor,
                            Box::new(move || {
                                se_host.socket_epoch_of(&se_peer).as_deref()
                                    == Some(se_val.as_str())
                            }),
                        );
                    });

                    let install_for_outer = install_once.clone();
                    *baseline_fn.lock().unwrap_or_else(PoisonError::into_inner) =
                        Some(Box::new(move || install_for_outer()));

                    let mut answer_map = subscribed.answer.as_object().cloned().unwrap_or_default();
                    answer_map.insert("sessionId".into(), Value::String(subscription_id.clone()));
                    answer_map.insert("admissionId".into(), Value::String(admission_id.clone()));
                    answer_map.insert("generation".into(), json!(0));
                    if let Some(data) = answer_map.get_mut("data").and_then(Value::as_object_mut) {
                        data.remove("snapshot");
                        data.insert(
                            "recovery".into(),
                            serde_json::to_value(&lease).unwrap_or(Value::Null),
                        );
                    }

                    let act_host = host.clone();
                    let act_sub = subscription_id.clone();
                    let act_install = install_once;

                    let ref_pending = pending;
                    let ref_adm = admission_id.clone();

                    let rel_live = live;
                    let rel_host = host;
                    let rel_peer = peer;
                    let rel_sub = subscription_id;
                    let rel_events = events;
                    let rel_se = socket_epoch;
                    let rel_lease_id = lease.id;

                    Ok(Produced {
                        answer: answer_map,
                        activate: Some(Box::new(move || {
                            act_install();
                            act_host.activate_status(&act_sub);
                            Ok(())
                        })),
                        refusal: Some(Box::new(move || {
                            let reason: MirrorPendingReason = ref_pending
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .reason?;
                            let err = error_response(
                                None,
                                "subscribe_sessions",
                                reason.as_str(),
                                Some(json!({
                                    "kind": "request_refused",
                                    "reason": reason.as_str(),
                                    "hint": "new_request",
                                })),
                            );
                            let mut map = err.as_object().cloned().unwrap_or_default();
                            map.insert("admissionId".into(), Value::String(ref_adm.clone()));
                            map.insert("generation".into(), json!(0));
                            Some(map)
                        })),
                        release: Some(Box::new(move || {
                            Box::pin(async move {
                                if !rel_live.swap(false, Ordering::SeqCst) {
                                    return Ok(());
                                }
                                rel_host.unregister_subscription(&rel_peer, &rel_sub);
                                rel_host.unsubscribe_status(&rel_peer, &rel_sub);
                                rel_events.abandon(&rel_se, &rel_lease_id);
                                Ok(())
                            })
                        })),
                    })
                })
            })
            .await
    };

    match run_res {
        Ok(answer) => {
            if host.socket_epoch_of(&peer).as_deref() != Some(socket_epoch.as_str()) {
                if let Some(tx) = delivery_tx
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
                {
                    let _ = tx.send(None);
                }
                return Ok(None);
            }
            let mut response = cancelled_answer().await.unwrap_or(answer);
            if let Some(id) = command.id {
                response.insert("id".into(), Value::String(id));
            } else {
                response.remove("id");
            }
            if let Some(tx) = delivery_tx
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                if let Some(bl) = baseline_fn
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
                {
                    bl();
                }
                let _ = tx.send(Some(response));
            } else {
                let _ = host.enqueue_control(Value::Object(response), &peer).await;
            }
            Ok(None)
        }
        Err(cause) => {
            let refusal = match cancelled_answer().await {
                Some(c) => Value::Object(c),
                None => error_response(
                    command.id.as_deref(),
                    "subscribe_sessions",
                    &format!("admission_{}", cause.reason.as_str()),
                    Some(json!({
                        "kind": "request_refused",
                        "reason": cause.reason.as_str(),
                        "hint": "new_request",
                    })),
                ),
            };
            if let Some(tx) = delivery_tx
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                if let Some(bl) = baseline_fn
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
                {
                    bl();
                }
                let _ = tx.send(refusal.as_object().cloned());
                return Ok(None);
            }
            Ok(Some(refusal))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_admission_baseline::LegacyViewCursor;

    struct MockSubHost {
        admissions: Arc<CompactAdmissions>,
        events: EventLeases,
        pending: Arc<Mutex<MirrorPending>>,
        activated: AtomicBool,
        baselines: Mutex<Vec<GeneratedFramesCursor>>,
    }

    impl SubscribeRouterHost for MockSubHost {
        fn current_connection(&self) -> Option<String> {
            Some("p1".into())
        }
        fn socket_epoch_of(&self, _: &str) -> Option<String> {
            Some("se-1".into())
        }
        fn recovery_events(&self) -> Option<EventLeases> {
            Some(self.events.clone())
        }
        fn admissions(&self) -> Option<Arc<CompactAdmissions>> {
            Some(self.admissions.clone())
        }
        fn answer_subscribe_sessions(&self, _: &Value, _: &str) -> SubscribeCommandResult {
            SubscribeCommandResult {
                answer: json!({
                    "type": "response",
                    "command": "subscribe_sessions",
                    "success": true,
                    "data": { "subscriptionId": "sub-1", "snapshot": [], "seq": 0 }
                }),
                activate_subscription_id: Some("sub-1".into()),
                snapshot_rows: vec![json!({"sessionId":"s1","status":"idle"})],
            }
        }
        fn register_subscription(&self, _: &str, _: &str, _: &str) {}
        fn unregister_subscription(&self, _: &str, _: &str) {}
        fn pending_buffer(&self, _: &str) -> Option<Arc<Mutex<MirrorPending>>> {
            Some(self.pending.clone())
        }
        fn activate_status(&self, _: &str) {
            self.activated.store(true, Ordering::SeqCst);
        }
        fn unsubscribe_status(&self, _: &str, _: &str) {}
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
        fn enqueue_control(&self, _: Value, _: &str) -> BoxFuture<'static, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn subscribes_with_event_lease_and_activates_pending() {
        let host = Arc::new(MockSubHost {
            admissions: Arc::new(CompactAdmissions::default()),
            events: EventLeases::default(),
            pending: Arc::new(Mutex::new(MirrorPending::new())),
            activated: AtomicBool::new(false),
            baselines: Mutex::new(Vec::new()),
        });
        let res = compact_subscribe(
            host.clone(),
            SubscribeCommandInput {
                id: Some("1".into()),
                admission_id: Some("adm-sub".into()),
                retry: false,
                raw_command: json!({"type":"subscribe_sessions"}),
                fingerprint_fields: BTreeMap::new(),
            },
        )
        .await
        .expect("subscribe");
        assert!(res.is_none());
        assert!(host.activated.load(Ordering::SeqCst));
        let mut bl = host
            .baselines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(0);
        let reply = bl.next_frame().await.expect("reply");
        assert!(reply.contains("\"kind\":\"event\""));
        let manifest = bl.next_frame().await.expect("manifest");
        assert!(manifest.contains("\"componentRef\":\"record\""));
        host.admissions.dispose().await;
    }
}
