//! Port of senpi `packages/coding-agent/src/modes/rpc/secret-input-presentations.ts`.
//!
//! Private presenter records contain identities only; output bypasses every replay and observer lane.

use crate::jsonl::serialize_json_line;
use crate::secret_input_requests::{
    SealedAnswerReply, SealedLatch, SealedOfferDetails, SecretInputRoute,
    SecretInputUnavailableReason,
};
use crate::secret_sealed::SecretInputOutcome;
use crate::secret_sealed_answer_route::{SealedAnswerTarget, answer_sealed_frame};
use crate::secret_sealed_holders::{SealedHolder, SealedSecretOffer, SealedSecretSource};
use crate::session_event_fanout::SessionEventFanout;
use crate::user_skill_approval::RpcRemoteOrigin;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const NO_RAISING_CONNECTION: &str = "agent";
pub const SECRET_INPUT_CAPABILITY: &str = "secret_input_v1";
pub const RAISER_NOTICE_CAPABILITY: &str = "raiser_notice_v1";

/// Where a connection speaks from (`ConnectionLocality` in `raiser-notices.ts`).
pub trait ConnectionLocality: Send + Sync {
    fn is_remote(&self, connection: &str) -> bool;
    fn origin_of(&self, connection: &str) -> Option<RpcRemoteOrigin>;
}

/// Default local-only [`ConnectionLocality`] when no relay origin registry is attached.
#[derive(Debug, Default)]
pub struct LocalOnlyLocality;

impl ConnectionLocality for LocalOnlyLocality {
    fn is_remote(&self, _connection: &str) -> bool {
        false
    }

    fn origin_of(&self, _connection: &str) -> Option<RpcRemoteOrigin> {
        None
    }
}

/// Value-free notices of a raiser-only dialog (`RaiserNotices` in `raiser-notices.ts`).
struct RaiserNotices {
    pending: HashMap<String, HashMap<String, Vec<String>>>,
}

impl RaiserNotices {
    fn new() -> Self {
        Self {
            pending: HashMap::new(),
        }
    }

    fn raised(
        &mut self,
        fanout: &SessionEventFanout,
        flush: &dyn Fn(),
        session_id: &str,
        ui_request_id: &str,
        excluded: &[Option<&str>],
        mut notice: Value,
    ) {
        let session = self.pending.entry(session_id.to_owned()).or_default();
        if session.contains_key(ui_request_id) {
            return;
        }
        let recipients: Vec<String> = fanout
            .targets
            .attached_connections(session_id)
            .into_iter()
            .filter(|id| {
                !excluded.contains(&Some(id.as_str()))
                    && fanout.targets.connection_has(id, RAISER_NOTICE_CAPABILITY)
            })
            .collect();
        if recipients.is_empty() {
            return;
        }
        session.insert(ui_request_id.to_owned(), recipients.clone());
        notice["sessionId"] = json!(session_id);
        notice["uiRequestId"] = json!(ui_request_id);
        if let Ok(line) = serialize_json_line(&notice) {
            for id in &recipients {
                let _ = fanout.enqueue_line(id, &line);
            }
            flush();
        }
    }

    fn settled(
        &mut self,
        fanout: &SessionEventFanout,
        flush: &dyn Fn(),
        session_id: &str,
        ui_request_id: &str,
        mut record: Value,
    ) {
        let Some(session) = self.pending.get_mut(session_id) else {
            return;
        };
        let Some(recipients) = session.remove(ui_request_id) else {
            return;
        };
        if session.is_empty() {
            self.pending.remove(session_id);
        }
        record["sessionId"] = json!(session_id);
        record["uiRequestId"] = json!(ui_request_id);
        if let Ok(line) = serialize_json_line(&record) {
            for id in &recipients {
                let _ = fanout.enqueue_line(id, &line);
            }
            flush();
        }
    }
}

struct SecretOwner {
    session: String,
    connection: Option<String>,
    cancel: Option<Box<dyn FnOnce() + Send + Sync>>,
    raiser: bool,
    sealed: Vec<SealedHolder>,
    background: bool,
    raised_by: String,
    origin: Option<RpcRemoteOrigin>,
}

struct PresentationInner {
    fanout: Arc<Mutex<SessionEventFanout>>,
    current: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    write_sole: Arc<dyn Fn(&str) + Send + Sync>,
    flush: Arc<dyn Fn() + Send + Sync>,
    locality: Arc<dyn ConnectionLocality>,
    notices: RaiserNotices,
    owners: HashMap<String, SecretOwner>,
    sole_capabilities: Vec<String>,
    sealed_source: Option<Arc<dyn SealedSecretSource>>,
    sealed_unsub: Option<Box<dyn FnOnce() + Send>>,
    latches: HashMap<String, Arc<dyn SealedLatch>>,
}

impl PresentationInner {
    fn capable(&self, fanout: &SessionEventFanout, connection: Option<&str>) -> bool {
        Self::owner_capable(&self.sole_capabilities, fanout, connection)
    }

    fn owner_capable(
        sole_capabilities: &[String],
        fanout: &SessionEventFanout,
        connection: Option<&str>,
    ) -> bool {
        let caps: Option<&[String]> = match connection {
            None => Some(sole_capabilities),
            Some(id) => fanout.targets.capabilities(id),
        };
        caps.is_some_and(|c| {
            c.iter().any(|x| x == SECRET_INPUT_CAPABILITY)
                && c.iter()
                    .any(|x| x == crate::custom_capability::INTERACTIVE_USER_CAPABILITY)
        })
    }

    fn local(&self, connection: &str) -> bool {
        !self.locality.is_remote(connection)
    }

    fn owner_live(
        sole_capabilities: &[String],
        locality: &dyn ConnectionLocality,
        fanout: &SessionEventFanout,
        owner: &SecretOwner,
    ) -> bool {
        if !Self::owner_capable(sole_capabilities, fanout, owner.connection.as_deref()) {
            return false;
        }
        match owner.connection.as_deref() {
            None => fanout.is_empty(),
            Some(conn) => {
                fanout
                    .attached_connections(&owner.session)
                    .iter()
                    .any(|c| c == conn)
                    && (!owner.background || !locality.is_remote(conn))
            }
        }
    }

    fn live(&self, fanout: &SessionEventFanout, owner: &SecretOwner) -> bool {
        Self::owner_live(
            &self.sole_capabilities,
            self.locality.as_ref(),
            fanout,
            owner,
        )
    }

    fn available(&self, fanout: &SessionEventFanout, owner: &SecretOwner) -> bool {
        (owner.raiser && self.live(fanout, owner))
            || owner
                .sealed
                .iter()
                .any(|h| self.sealed_source.as_ref().is_some_and(|s| s.current(h)))
    }

    fn recheck(&mut self) {
        let to_cancel: Vec<Box<dyn FnOnce() + Send + Sync>> = {
            let fanout = self
                .fanout
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let unavailable_ids: Vec<String> = self
                .owners
                .iter()
                .filter(|(_, owner)| !self.available(&fanout, owner))
                .map(|(id, _)| id.clone())
                .collect();
            unavailable_ids
                .into_iter()
                .filter_map(|id| self.owners.get_mut(&id).and_then(|o| o.cancel.take()))
                .collect()
        };
        for cancel in to_cancel {
            cancel();
        }
    }
}

#[derive(Clone)]
pub struct SecretInputPresentations {
    inner: Arc<Mutex<PresentationInner>>,
}

impl SecretInputPresentations {
    pub fn new(
        fanout: Arc<Mutex<SessionEventFanout>>,
        current: Arc<dyn Fn() -> Option<String> + Send + Sync>,
        write_sole: Arc<dyn Fn(&str) + Send + Sync>,
        flush: Arc<dyn Fn() + Send + Sync>,
        locality: Arc<dyn ConnectionLocality>,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PresentationInner {
                fanout,
                current,
                write_sole,
                flush,
                locality,
                notices: RaiserNotices::new(),
                owners: HashMap::new(),
                sole_capabilities: Vec::new(),
                sealed_source: None,
                sealed_unsub: None,
                latches: HashMap::new(),
            })),
        }
    }

    pub fn handshake(&self, capabilities: &[String]) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.sole_capabilities = capabilities.to_vec();
        inner.recheck();
    }

    pub fn use_sealed(
        &self,
        source: Option<Arc<dyn SealedSecretSource>>,
    ) -> Box<dyn FnOnce() + Send> {
        let weak = Arc::downgrade(&self.inner);
        let source_ptr = source
            .as_ref()
            .map(|s| Arc::as_ptr(s).cast::<()>() as usize);
        {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(unsub) = inner.sealed_unsub.take() {
                unsub();
            }
            inner.sealed_source = source.clone();
            inner.sealed_unsub = source.as_ref().map(|src| {
                let this = self.clone();
                src.on_change(Arc::new(move || this.recheck()))
            });
            inner.recheck();
        }
        Box::new(move || {
            let Some(arc) = weak.upgrade() else {
                return;
            };
            let mut inner = arc
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let current_ptr = inner
                .sealed_source
                .as_ref()
                .map(|s| Arc::as_ptr(s).cast::<()>() as usize);
            if current_ptr == source_ptr {
                if let Some(unsub) = inner.sealed_unsub.take() {
                    unsub();
                }
                inner.sealed_source = None;
                inner.recheck();
            }
        })
    }

    pub fn route(&self, session: impl Into<String>, sealed: bool) -> Arc<dyn SecretInputRoute> {
        Arc::new(PresentationRoute {
            presentations: self.clone(),
            session: session.into(),
            sealed,
            bound_latch: Mutex::new(None),
        })
    }

    pub async fn answer_sealed(&self, connection: &str, value: &Value) -> SealedAnswerReply {
        let source = {
            self.inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .sealed_source
                .clone()
        };
        answer_sealed_frame(connection, value, source.as_ref(), |id| {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let owner = inner.owners.get(id)?;
            Some(SealedAnswerTarget {
                sealed: owner.sealed.clone(),
                latch: inner.latches.get(&owner.session).cloned(),
            })
        })
        .await
    }

    pub fn recheck(&self) {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recheck();
    }

    /// The raiser leaves; the request ends only when no sealed holder is current either.
    pub fn detach(&self, connection: &str) {
        let cancels: Vec<Box<dyn FnOnce() + Send + Sync>> = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let fanout = inner.fanout.clone();
            let fanout_guard = fanout
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let matching_ids: Vec<String> = inner
                .owners
                .iter()
                .filter(|(_, o)| o.connection.as_deref() == Some(connection))
                .map(|(id, _)| id.clone())
                .collect();
            let sole_capabilities = inner.sole_capabilities.clone();
            let locality = Arc::clone(&inner.locality);
            let sealed_source = inner.sealed_source.clone();
            let mut to_cancel = Vec::new();
            let mut to_remove = Vec::new();
            for id in matching_ids {
                let keep = if let Some(owner) = inner.owners.get_mut(&id) {
                    owner.raiser = false;
                    owner.connection = None;
                    let has_sealed = owner
                        .sealed
                        .iter()
                        .any(|h| sealed_source.as_ref().is_some_and(|s| s.current(h)));
                    let live_raiser = owner.raiser
                        && PresentationInner::owner_live(
                            &sole_capabilities,
                            locality.as_ref(),
                            &fanout_guard,
                            owner,
                        );
                    has_sealed || live_raiser
                } else {
                    true
                };
                if !keep && let Some(mut owner) = inner.owners.remove(&id) {
                    if let Some(c) = owner.cancel.take() {
                        to_cancel.push(c);
                    }
                    to_remove.push(id);
                }
            }
            drop(fanout_guard);
            let _ = to_remove;
            to_cancel
        };
        for cancel in cancels {
            cancel();
        }
    }
}

struct PresentationRoute {
    presentations: SecretInputPresentations,
    session: String,
    sealed: bool,
    bound_latch: Mutex<Option<usize>>,
}

impl SecretInputRoute for PresentationRoute {
    fn select(
        &self,
        id: &str,
        unavailable: Box<dyn FnOnce() + Send + Sync>,
    ) -> Option<SecretInputUnavailableReason> {
        let mut inner = self
            .presentations
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let preferred = (inner.current)();
        let fanout_arc = inner.fanout.clone();
        let fanout = fanout_arc
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let attached = fanout.attached_connections(&self.session);
        let mut refused: Option<SecretInputUnavailableReason> = None;
        let connection = preferred.clone().or_else(|| {
            attached
                .iter()
                .find(|candidate| inner.capable(&fanout, Some(candidate)) && inner.local(candidate))
                .cloned()
        });
        if let Some(ref pref) = preferred {
            if !attached.contains(pref) || !inner.capable(&fanout, Some(pref)) {
                refused = Some(SecretInputUnavailableReason::ClientUnsupported);
            }
        } else if connection.is_none() && !(fanout.is_empty() && inner.capable(&fanout, None)) {
            refused = Some(if fanout.is_empty() {
                SecretInputUnavailableReason::ClientUnsupported
            } else {
                SecretInputUnavailableReason::NoPresenter
            });
        }
        let holders = if self.sealed {
            inner
                .sealed_source
                .as_ref()
                .map(|s| s.holders(&self.session))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if let Some(reason) = refused
            && holders.is_empty()
        {
            return Some(reason);
        }
        let origin = preferred
            .as_deref()
            .and_then(|c| inner.locality.origin_of(c));
        inner.owners.insert(
            id.to_owned(),
            SecretOwner {
                session: self.session.clone(),
                connection: if refused.is_none() { connection } else { None },
                cancel: Some(unavailable),
                raiser: refused.is_none(),
                sealed: holders,
                background: preferred.is_none(),
                raised_by: preferred.unwrap_or_else(|| NO_RAISING_CONNECTION.to_owned()),
                origin,
            },
        );
        None
    }

    fn accepts(&self, id: &str) -> bool {
        let inner = self
            .presentations
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(owner) = inner.owners.get(id) else {
            return false;
        };
        if owner.session != self.session || !owner.raiser {
            return false;
        }
        if owner.connection != (inner.current)() {
            return false;
        }
        let fanout = inner
            .fanout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.live(&fanout, owner)
    }

    fn available(&self, id: &str) -> bool {
        let inner = self
            .presentations
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(owner) = inner.owners.get(id) else {
            return false;
        };
        if owner.session != self.session {
            return false;
        }
        let fanout = inner
            .fanout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.available(&fanout, owner)
    }

    fn presenter(&self, id: &str) -> Option<String> {
        self.presentations
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .owners
            .get(id)
            .and_then(|o| o.connection.clone())
    }

    fn sealed(&self, id: &str) -> bool {
        self.presentations
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .owners
            .get(id)
            .is_some_and(|o| !o.sealed.is_empty())
    }

    fn raiser(&self, id: &str) -> bool {
        self.presentations
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .owners
            .get(id)
            .is_some_and(|o| o.raiser)
    }

    fn sealed_outcome(&self, id: &str, outcome: SecretInputOutcome, winner_grant: Option<&str>) {
        let inner = self
            .presentations
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let (Some(owner), Some(source)) = (inner.owners.get(id), &inner.sealed_source)
            && owner.session == self.session
            && !owner.sealed.is_empty()
        {
            source.settle(&owner.sealed, id, outcome, winner_grant);
        }
    }

    fn bind_sealed_responder(&self, latch: Option<Arc<dyn SealedLatch>>) {
        let mut inner = self
            .presentations
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut bound = self
            .bound_latch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match latch {
            None => {
                if let Some(ptr) = bound.take()
                    && inner
                        .latches
                        .get(&self.session)
                        .is_some_and(|l| Arc::as_ptr(l).cast::<()>() as usize == ptr)
                {
                    inner.latches.remove(&self.session);
                }
            }
            Some(l) if self.sealed => {
                *bound = Some(Arc::as_ptr(&l).cast::<()>() as usize);
                inner.latches.insert(self.session.clone(), l);
            }
            Some(_) => {}
        }
    }

    fn offer_sealed(&self, id: &str, offer: SealedOfferDetails) {
        let inner = self
            .presentations
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let (Some(owner), Some(source)) = (inner.owners.get(id), &inner.sealed_source)
            && owner.session == self.session
            && !owner.sealed.is_empty()
        {
            source.offer(
                &self.session,
                &owner.sealed,
                SealedSecretOffer {
                    ui_request_id: id.to_owned(),
                    title: offer.title,
                    placeholder: offer.placeholder,
                    deadline_at_ms: offer.deadline_at_ms,
                    hpke_pk: offer.hpke_pk,
                },
            );
        }
    }

    fn send(&self, id: &str, mut record: Value) {
        let (cancel, line, conn, flush) = {
            let mut inner = self
                .presentations
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let fanout_arc = inner.fanout.clone();
            let fanout = fanout_arc
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let flush = inner.flush.clone();

            // Notify non-presenters first.
            let notice_info = inner
                .owners
                .get(id)
                .filter(|o| o.session == self.session)
                .map(|o| {
                    let avail = inner.available(&fanout, o);
                    let holder_conns: Vec<String> = inner
                        .sealed_source
                        .as_ref()
                        .map(|src| o.sealed.iter().map(|h| src.connection_of(h)).collect())
                        .unwrap_or_default();
                    let raising = if o.raised_by == NO_RAISING_CONNECTION {
                        None
                    } else {
                        Some(o.raised_by.clone())
                    };
                    (
                        avail,
                        o.connection.clone(),
                        raising,
                        holder_conns,
                        o.raised_by.clone(),
                        o.origin.clone(),
                    )
                });

            if let Some((avail, owner_conn, raising, holder_conns, raised_by, origin)) = notice_info
            {
                let rec_type = record.get("type").and_then(Value::as_str);
                if rec_type == Some("extension_ui_request") && avail {
                    let mut excluded_strs: Vec<String> = Vec::new();
                    if let Some(c) = owner_conn {
                        excluded_strs.push(c);
                    }
                    if let Some(r) = raising {
                        excluded_strs.push(r);
                    }
                    excluded_strs.extend(holder_conns);
                    let excluded: Vec<Option<&str>> =
                        excluded_strs.iter().map(|s| Some(s.as_str())).collect();
                    let mut notice = json!({
                        "type": "secret_input_notice",
                        "title": record.get("title"),
                        "deadlineAtMs": record.get("deadlineAtMs"),
                        "raisedBy": raised_by,
                    });
                    if let Some(orig) = origin {
                        notice["remoteOrigin"] = serde_json::to_value(orig).unwrap_or(Value::Null);
                    }
                    inner.notices.raised(
                        &fanout,
                        flush.as_ref(),
                        &self.session,
                        id,
                        &excluded,
                        notice,
                    );
                } else if rec_type == Some("secret_input_settled") {
                    inner.notices.settled(
                        &fanout,
                        flush.as_ref(),
                        &self.session,
                        id,
                        json!({
                            "type": "secret_input_notice_settled",
                            "outcome": record.get("outcome"),
                        }),
                    );
                }
            }

            let available = inner
                .owners
                .get(id)
                .is_some_and(|o| o.session == self.session && inner.available(&fanout, o));
            if !available {
                let cancel = inner.owners.get_mut(id).and_then(|o| o.cancel.take());
                (cancel, None, None, flush)
            } else {
                let live_raiser = inner
                    .owners
                    .get(id)
                    .is_some_and(|o| o.raiser && inner.live(&fanout, o));
                if !live_raiser {
                    (None, None, None, flush)
                } else {
                    record["sessionId"] = json!(self.session);
                    let line = serialize_json_line(&record).ok();
                    let conn = inner.owners.get(id).and_then(|o| o.connection.clone());
                    (None, line, conn, flush)
                }
            }
        };
        if let Some(cancel) = cancel {
            cancel();
            return;
        }
        if let Some(line) = line {
            match conn {
                None => {
                    let write_sole = self
                        .presentations
                        .inner
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .write_sole
                        .clone();
                    write_sole(&line);
                }
                Some(ref cid) => {
                    let fanout_arc = self
                        .presentations
                        .inner
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .fanout
                        .clone();
                    let fanout = fanout_arc
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let _ = fanout.enqueue_line(cid, &line);
                }
            }
            flush();
        }
    }

    fn settled(&self, id: &str) {
        let mut inner = self
            .presentations
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(owner) = inner.owners.get_mut(id) {
            owner.cancel = None;
        }
        if let Some(source) = &inner.sealed_source {
            source.retire(id);
        }
    }

    fn forget(&self, id: &str) {
        let mut inner = self
            .presentations
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.owners.remove(id);
        if let Some(source) = &inner.sealed_source {
            source.withdraw(id);
        }
    }

    fn handshake(&self, capabilities: &[String]) {
        let current = {
            self.presentations
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .current
                .clone()
        };
        if current().is_none() {
            self.presentations.handshake(capabilities);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sole_handshake_gates_capability_and_detach_cancels_when_no_sealed_holders() {
        let fanout = Arc::new(Mutex::new(SessionEventFanout::default()));
        let pres = SecretInputPresentations::new(
            fanout,
            Arc::new(|| None),
            Arc::new(|_| {}),
            Arc::new(|| {}),
            Arc::new(LocalOnlyLocality),
        );
        let route = pres.route("s1", false);
        assert_eq!(
            route.select("r1", Box::new(|| {})),
            Some(SecretInputUnavailableReason::ClientUnsupported)
        );
        route.handshake(&[
            SECRET_INPUT_CAPABILITY.into(),
            crate::custom_capability::INTERACTIVE_USER_CAPABILITY.into(),
        ]);
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let c_flag = cancelled.clone();
        assert_eq!(
            route.select(
                "r2",
                Box::new(move || c_flag.store(true, std::sync::atomic::Ordering::SeqCst))
            ),
            None
        );
        assert!(route.accepts("r2"));
        pres.handshake(&[]);
        assert!(cancelled.load(std::sync::atomic::Ordering::SeqCst));
    }
}
