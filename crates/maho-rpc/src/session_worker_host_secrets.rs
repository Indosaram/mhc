//! Port of senpi `packages/coding-agent/src/modes/rpc/session-worker-host-secrets.ts`.
//!
//! The host half of one worker isolate's secret prompts (senpi#5192; D11 secret phase 5.5): the
//! route selection the isolate asks for, its bounded frames, and sealed answers forwarded to the
//! isolate's latch after the host authenticated them.

use crate::secret_input_requests::{
    SealedAnswer, SealedAnswerError, SealedAnswerReply, SealedClaim, SealedLatch,
    SealedOfferDetails, SecretInputRoute,
};
use crate::secret_sealed::SecretInputOutcome;
use crate::session_worker_secrets::{WorkerSecretIds, is_sealed_offer};
use crate::session_worker_signals::WorkerSignal;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// The most answers for one request in flight or queued toward the isolate (D11 secret phase 5.5).
const SEALED_ANSWERS_PER_ID: usize = 5;

/// The isolate's reply is untrusted (#5192): only the shapes its latch can produce pass.
pub fn isolate_reply(value: &Value) -> SealedAnswerReply {
    SealedAnswerReply::from_wire(value).unwrap_or(SealedAnswerReply::Error(
        SealedAnswerError::SecretAnswerRefused,
    ))
}

/// Host <-> worker secret messages on the worker control lane.
pub enum HostSecretMessage {
    Select {
        id: String,
        connection: Option<String>,
        decision: Arc<Mutex<Option<Value>>>,
        signal: WorkerSignal,
    },
    Frame {
        id: String,
        record: Value,
        signal: WorkerSignal,
    },
    Settled {
        id: String,
        outcome: Option<SecretInputOutcome>,
        winner_grant: Option<String>,
        signal: WorkerSignal,
    },
    Forget {
        id: String,
        signal: WorkerSignal,
    },
    Handshake {
        capabilities: Vec<String>,
        signal: WorkerSignal,
    },
}

/// Writer/session surface that `WorkerHostSecrets` binds to.
pub trait HostSecretsWriter: Send + Sync {
    fn secret_input_route(&self, session_id: &str, sealed: bool) -> Arc<dyn SecretInputRoute>;
    fn with_connection(&self, connection: &str, action: Box<dyn FnOnce() + Send>);
    fn set_sole_secret_capabilities(&self, capabilities: &[String]);
}

pub trait HostSecretsDeps: Send + Sync {
    fn post_cancel_secret(&self, id: &str);
    fn request_sealed_answer(
        &self,
        answer: SealedAnswer,
    ) -> BoxFuture<'static, Result<Value, String>>;
}

struct QueueSlot {
    lock: Arc<tokio::sync::Mutex<()>>,
    depth: usize,
}

struct Inner {
    route: Option<Arc<dyn SecretInputRoute>>,
    writer: Option<Arc<dyn HostSecretsWriter>>,
    ids: WorkerSecretIds,
    queue: HashMap<String, QueueSlot>,
    forwarded: HashMap<String, HashSet<String>>,
    settled_ids: HashSet<String>,
    deps: Arc<dyn HostSecretsDeps>,
}

/// Host half of one worker isolate's secret prompts.
#[derive(Clone)]
pub struct WorkerHostSecrets {
    inner: Arc<Mutex<Inner>>,
}

impl WorkerHostSecrets {
    pub fn new(deps: Arc<dyn HostSecretsDeps>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                route: None,
                writer: None,
                ids: WorkerSecretIds::new(),
                queue: HashMap::new(),
                forwarded: HashMap::new(),
                settled_ids: HashSet::new(),
                deps,
            })),
        }
    }

    pub fn bind(&self, session_id: &str, writer: Arc<dyn HostSecretsWriter>) {
        let route = writer.secret_input_route(session_id, true);
        let latch: Arc<dyn SealedLatch> = Arc::new(HostForwardLatch { host: self.clone() });
        route.bind_sealed_responder(Some(latch));
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.writer = Some(writer);
        inner.route = Some(route);
    }

    /// A forwarded `extension_ui_response` for one of this isolate's secrets that its route does
    /// not accept.
    pub fn refuses(&self, ui_request_id: &str) -> bool {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.ids.has(ui_request_id)
            && !inner
                .route
                .as_ref()
                .is_some_and(|r| r.accepts(ui_request_id))
    }

    pub fn exited(&self) {
        let (route, unsettled, all_ids) = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let route = inner.route.take();
            let all_ids = inner.ids.ids();
            let unsettled: Vec<String> = all_ids
                .iter()
                .filter(|id| !inner.settled_ids.contains(*id))
                .cloned()
                .collect();
            inner.ids.clear();
            inner.forwarded.clear();
            inner.settled_ids.clear();
            inner.queue.clear();
            (route, unsettled, all_ids)
        };
        if let Some(route) = route {
            route.bind_sealed_responder(None);
            for id in &unsettled {
                route.sealed_outcome(id, SecretInputOutcome::Unavailable, None);
                route.settled(id);
            }
            for id in &all_ids {
                route.forget(id);
            }
        }
    }

    pub fn receive(&self, message: HostSecretMessage) -> bool {
        match message {
            HostSecretMessage::Select {
                id,
                connection,
                decision,
                signal,
            } => {
                self.select(id, connection, decision, signal);
                true
            }
            HostSecretMessage::Frame { id, record, signal } => {
                let mut inner = self
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let is_offer_type =
                    record.get("type").and_then(Value::as_str) == Some("secret_sealed_offer");
                let parsed_offer = is_sealed_offer(&record);
                if (is_offer_type && parsed_offer.is_none()) || !inner.ids.admit_frame(&id, &record)
                {
                    signal.acknowledge(false);
                    return true;
                }
                let route = inner.route.clone();
                drop(inner);
                if let Some(route) = route {
                    if let Some(offer) = parsed_offer {
                        route.offer_sealed(
                            &id,
                            SealedOfferDetails {
                                hpke_pk: offer.hpke_pk,
                                title: offer.title,
                                placeholder: offer.placeholder,
                                deadline_at_ms: offer.deadline_at_ms,
                            },
                        );
                    } else {
                        route.send(&id, record);
                    }
                }
                signal.acknowledge(true);
                true
            }
            HostSecretMessage::Settled {
                id,
                outcome,
                winner_grant,
                signal,
            } => {
                let mut inner = self
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if !Self::admits_settle(&inner, &id, outcome, winner_grant.as_deref()) {
                    signal.acknowledge(false);
                    return true;
                }
                inner.settled_ids.insert(id.clone());
                let route = inner.route.clone();
                drop(inner);
                if let Some(route) = route {
                    if let Some(outcome) = outcome {
                        route.sealed_outcome(&id, outcome, winner_grant.as_deref());
                    }
                    route.settled(&id);
                }
                signal.acknowledge(true);
                true
            }
            HostSecretMessage::Forget { id, signal } => {
                let mut inner = self
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                inner.ids.forget(&id);
                inner.forwarded.remove(&id);
                inner.settled_ids.remove(&id);
                let route = inner.route.clone();
                drop(inner);
                if let Some(route) = route {
                    route.forget(&id);
                }
                signal.acknowledge(true);
                true
            }
            HostSecretMessage::Handshake {
                capabilities,
                signal,
            } => {
                let writer = self
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .writer
                    .clone();
                if let Some(writer) = writer {
                    writer.set_sole_secret_capabilities(&capabilities);
                }
                signal.acknowledge(true);
                true
            }
        }
    }

    fn select(
        &self,
        id: String,
        connection: Option<String>,
        decision: Arc<Mutex<Option<Value>>>,
        signal: WorkerSignal,
    ) {
        let (admit, writer) = {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let admit = inner.ids.has(&id) || inner.ids.admits_new();
            (admit, inner.writer.clone())
        };
        if !admit {
            signal.acknowledge(false);
            return;
        }
        let this = self.clone();
        let id_cloned = id.clone();
        let run = Box::new(move || {
            let (route, deps) = {
                let inner = this
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                (inner.route.clone(), inner.deps.clone())
            };
            let reason = match &route {
                None => {
                    Some(crate::secret_input_requests::SecretInputUnavailableReason::NoPresenter)
                }
                Some(r) => {
                    let id_cancel = id_cloned.clone();
                    r.select(
                        &id_cloned,
                        Box::new(move || deps.post_cancel_secret(&id_cancel)),
                    )
                }
            };
            if reason.is_none() {
                this.inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .ids
                    .add(&id_cloned);
            }
            let result = match (reason, &route) {
                (None, Some(r)) => json!({
                    "accepted": true,
                    "connection": r.presenter(&id_cloned),
                    "raiser": r.raiser(&id_cloned),
                    "sealed": r.sealed(&id_cloned),
                }),
                (None, None) => json!({
                    "accepted": true,
                    "raiser": true,
                    "sealed": false,
                }),
                (Some(r), _) => json!({
                    "accepted": false,
                    "reason": r.as_str(),
                }),
            };
            *decision
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
            signal.acknowledge(true);
        });
        match (connection, writer) {
            (Some(conn), Some(w)) => w.with_connection(&conn, run),
            _ => run(),
        }
    }

    fn admits_settle(
        inner: &Inner,
        id: &str,
        outcome: Option<SecretInputOutcome>,
        winner_grant: Option<&str>,
    ) -> bool {
        if !inner.ids.has(id) || inner.settled_ids.contains(id) {
            return false;
        }
        let decided = matches!(
            outcome,
            Some(SecretInputOutcome::Answered | SecretInputOutcome::Cancelled)
        );
        match winner_grant {
            None => true,
            Some(wg) => {
                decided
                    && inner
                        .forwarded
                        .get(id)
                        .is_some_and(|grants| grants.contains(wg))
            }
        }
    }

    async fn forward(&self, answer: SealedAnswer, claim: &dyn SealedClaim) -> SealedAnswerReply {
        let id = answer.ui_request_id.clone();
        let (lock, deps) = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !inner.ids.has(&id) {
                return SealedAnswerReply::Error(SealedAnswerError::SecretAnswerRefused);
            }
            let current_depth = inner.queue.get(&id).map_or(0, |s| s.depth);
            if current_depth >= SEALED_ANSWERS_PER_ID {
                return SealedAnswerReply::Error(SealedAnswerError::SecretAnswerRefused);
            }
            let slot = inner.queue.entry(id.clone()).or_insert_with(|| QueueSlot {
                lock: Arc::new(tokio::sync::Mutex::new(())),
                depth: 0,
            });
            slot.depth += 1;
            (slot.lock.clone(), inner.deps.clone())
        };

        struct SlotCleanup<'a>(&'a WorkerHostSecrets, &'a str);
        impl Drop for SlotCleanup<'_> {
            fn drop(&mut self) {
                let mut inner = self
                    .0
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(slot) = inner.queue.get_mut(self.1) {
                    slot.depth = slot.depth.saturating_sub(1);
                    if slot.depth == 0 {
                        inner.queue.remove(self.1);
                    }
                }
            }
        }
        let _cleanup = SlotCleanup(self, &id);
        let _guard = lock.lock().await;
        if !claim.current() {
            return SealedAnswerReply::Error(SealedAnswerError::SecretAnswerRefused);
        }
        {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            inner
                .forwarded
                .entry(id.clone())
                .or_default()
                .insert(answer.grant_id.clone());
        }
        match deps.request_sealed_answer(answer).await {
            Ok(reply) => isolate_reply(&reply),
            Err(_) => SealedAnswerReply::Error(SealedAnswerError::SecretAnswerRefused),
        }
    }
}

struct HostForwardLatch {
    host: WorkerHostSecrets,
}

impl SealedLatch for HostForwardLatch {
    fn settled(
        &self,
        _ui_request_id: &str,
        _answer_id: &str,
        _principal: &str,
    ) -> Option<SealedAnswerReply> {
        None
    }

    fn respond<'a>(
        &'a self,
        answer: SealedAnswer,
        claim: &'a dyn SealedClaim,
    ) -> BoxFuture<'a, SealedAnswerReply> {
        Box::pin(self.host.forward(answer, claim))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret_input_requests::SecretInputUnavailableReason;

    struct DummyRoute;
    impl SecretInputRoute for DummyRoute {
        fn select(
            &self,
            _: &str,
            _: Box<dyn FnOnce() + Send + Sync>,
        ) -> Option<SecretInputUnavailableReason> {
            None
        }
        fn accepts(&self, _: &str) -> bool {
            false
        }
        fn available(&self, _: &str) -> bool {
            true
        }
        fn presenter(&self, _: &str) -> Option<String> {
            Some("c1".into())
        }
        fn send(&self, _: &str, _: Value) {}
        fn settled(&self, _: &str) {}
        fn forget(&self, _: &str) {}
    }

    struct DummyWriter;
    impl HostSecretsWriter for DummyWriter {
        fn secret_input_route(&self, _: &str, _: bool) -> Arc<dyn SecretInputRoute> {
            Arc::new(DummyRoute)
        }
        fn with_connection(&self, _: &str, action: Box<dyn FnOnce() + Send>) {
            action();
        }
        fn set_sole_secret_capabilities(&self, _: &[String]) {}
    }

    struct DummyDeps;
    impl HostSecretsDeps for DummyDeps {
        fn post_cancel_secret(&self, _: &str) {}
        fn request_sealed_answer(
            &self,
            _: SealedAnswer,
        ) -> BoxFuture<'static, Result<Value, String>> {
            Box::pin(async { Ok(json!({ "disposition": "applied" })) })
        }
    }

    #[test]
    fn admits_select_and_rejects_unforwarded_winner_grant_on_settle() {
        let host = WorkerHostSecrets::new(Arc::new(DummyDeps));
        host.bind("s1", Arc::new(DummyWriter));
        let decision = Arc::new(Mutex::new(None));
        let sig = WorkerSignal::default();
        assert!(host.receive(HostSecretMessage::Select {
            id: "req-1".into(),
            connection: Some("c1".into()),
            decision: decision.clone(),
            signal: sig.clone(),
        }));
        assert_eq!(sig.wait(std::time::Duration::ZERO), 1);
        assert_eq!(
            decision
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .expect("decision")["accepted"],
            true
        );
        assert!(host.refuses("req-1"));

        // Settling with an unforwarded winner grant must be refused.
        let settle_sig = WorkerSignal::default();
        host.receive(HostSecretMessage::Settled {
            id: "req-1".into(),
            outcome: Some(SecretInputOutcome::Answered),
            winner_grant: Some("forged-grant".into()),
            signal: settle_sig.clone(),
        });
        assert_eq!(settle_sig.wait(std::time::Duration::ZERO), 2);
    }
}
