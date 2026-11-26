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
        session_id: &str,
        ui_request_id: &str,
        excluded: &[Option<&str>],
        mut notice: Value,
    ) -> bool {
        let session = self.pending.entry(session_id.to_owned()).or_default();
        if session.contains_key(ui_request_id) {
            return false;
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
            return false;
        }
        session.insert(ui_request_id.to_owned(), recipients.clone());
        notice["sessionId"] = json!(session_id);
        notice["uiRequestId"] = json!(ui_request_id);
        if let Ok(line) = serialize_json_line(&notice) {
            for id in &recipients {
                let _ = fanout.enqueue_line(id, &line);
            }
            return true;
        }
        false
    }

    fn settled(
        &mut self,
        fanout: &SessionEventFanout,
        session_id: &str,
        ui_request_id: &str,
        mut record: Value,
    ) -> bool {
        let Some(session) = self.pending.get_mut(session_id) else {
            return false;
        };
        let Some(recipients) = session.remove(ui_request_id) else {
            return false;
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
            return true;
        }
        false
    }
}

struct SecretOwner {
    identity: Arc<()>,
    session: String,
    connection: Option<String>,
    cancel: Option<Box<dyn FnOnce() + Send + Sync>>,
    raiser: bool,
    sealed: Vec<SealedHolder>,
    background: bool,
    raised_by: String,
    origin: Option<RpcRemoteOrigin>,
}

impl SecretOwner {
    fn snapshot(&self) -> Self {
        Self {
            identity: Arc::clone(&self.identity),
            session: self.session.clone(),
            connection: self.connection.clone(),
            cancel: None,
            raiser: self.raiser,
            sealed: self.sealed.clone(),
            background: self.background,
            raised_by: self.raised_by.clone(),
            origin: self.origin.clone(),
        }
    }

    fn matches_snapshot(&self, snapshot: &Self) -> bool {
        Arc::ptr_eq(&self.identity, &snapshot.identity)
            && self.connection == snapshot.connection
            && self.raiser == snapshot.raiser
            && self.sealed == snapshot.sealed
            && self.background == snapshot.background
    }
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
    sealed_generation: Arc<()>,
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
                sealed_generation: Arc::new(()),
                latches: HashMap::new(),
            })),
        }
    }

    pub fn handshake(&self, capabilities: &[String]) {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sole_capabilities = capabilities.to_vec();
        self.recheck();
    }

    pub fn use_sealed(
        &self,
        source: Option<Arc<dyn SealedSecretSource>>,
    ) -> Box<dyn FnOnce() + Send> {
        let weak = Arc::downgrade(&self.inner);
        let generation = Arc::new(());
        let old_unsub = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let old_unsub = inner.sealed_unsub.take();
            inner.sealed_generation = Arc::clone(&generation);
            inner.sealed_source = source.clone();
            old_unsub
        };
        if let Some(unsub) = old_unsub {
            unsub();
        }
        self.recheck();
        let new_unsub = source.as_ref().map(|src| {
            let this = self.clone();
            src.on_change(Arc::new(move || this.recheck()))
        });
        let (stale_unsub, installed) = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if Arc::ptr_eq(&inner.sealed_generation, &generation) {
                inner.sealed_unsub = new_unsub;
                (None, true)
            } else {
                (new_unsub, false)
            }
        };
        if let Some(unsub) = stale_unsub {
            unsub();
        }
        if installed {
            self.recheck();
        }
        let this = self.clone();
        Box::new(move || {
            let Some(arc) = weak.upgrade() else {
                return;
            };
            let (unsub, removed) = {
                let mut inner = arc
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if Arc::ptr_eq(&inner.sealed_generation, &generation) {
                    let unsub = inner.sealed_unsub.take();
                    inner.sealed_source = None;
                    inner.sealed_generation = Arc::new(());
                    (unsub, true)
                } else {
                    (None, false)
                }
            };
            if let Some(unsub) = unsub {
                unsub();
            }
            if removed {
                this.recheck();
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
        let (fanout, capabilities, locality, source, generation, owners) = {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                Arc::clone(&inner.fanout),
                inner.sole_capabilities.clone(),
                Arc::clone(&inner.locality),
                inner.sealed_source.clone(),
                Arc::clone(&inner.sealed_generation),
                inner
                    .owners
                    .iter()
                    .filter(|(_, owner)| owner.cancel.is_some())
                    .map(|(id, owner)| (id.clone(), owner.snapshot()))
                    .collect::<Vec<_>>(),
            )
        };
        let unavailable = owners
            .into_iter()
            .filter(|(_, owner)| {
                let live = owner.raiser && {
                    let fanout = fanout
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    PresentationInner::owner_live(&capabilities, locality.as_ref(), &fanout, owner)
                };
                !live
                    && !owner
                        .sealed
                        .iter()
                        .any(|holder| source.as_ref().is_some_and(|s| s.current(holder)))
            })
            .collect::<Vec<_>>();
        let cancels = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !Arc::ptr_eq(&inner.sealed_generation, &generation)
                || inner.sole_capabilities != capabilities
            {
                Vec::new()
            } else {
                unavailable
                    .into_iter()
                    .filter_map(|(id, snapshot)| {
                        inner
                            .owners
                            .get_mut(&id)
                            .filter(|owner| owner.matches_snapshot(&snapshot))
                            .and_then(|owner| owner.cancel.take())
                    })
                    .collect::<Vec<_>>()
            }
        };
        for cancel in cancels {
            cancel();
        }
    }

    /// The raiser leaves; the request ends only when no sealed holder is current either.
    pub fn detach(&self, connection: &str) {
        let (source, generation, owners) = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let owners = inner
                .owners
                .iter_mut()
                .filter(|(_, o)| o.connection.as_deref() == Some(connection))
                .map(|(id, owner)| {
                    owner.raiser = false;
                    owner.connection = None;
                    (id.clone(), owner.snapshot())
                })
                .collect::<Vec<_>>();
            (
                inner.sealed_source.clone(),
                Arc::clone(&inner.sealed_generation),
                owners,
            )
        };
        let unavailable = owners
            .into_iter()
            .filter(|(_, owner)| {
                !owner
                    .sealed
                    .iter()
                    .any(|holder| source.as_ref().is_some_and(|s| s.current(holder)))
            })
            .collect::<Vec<_>>();
        let cancels = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !Arc::ptr_eq(&inner.sealed_generation, &generation) {
                Vec::new()
            } else {
                unavailable
                    .into_iter()
                    .filter_map(|(id, snapshot)| {
                        if inner
                            .owners
                            .get(&id)
                            .is_some_and(|owner| owner.matches_snapshot(&snapshot))
                        {
                            inner
                                .owners
                                .remove(&id)
                                .and_then(|mut owner| owner.cancel.take())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
            }
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
        let (mut inner, preferred, holders) = loop {
            let (current, source, generation) = {
                let inner = self
                    .presentations
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                (
                    Arc::clone(&inner.current),
                    inner.sealed_source.clone(),
                    Arc::clone(&inner.sealed_generation),
                )
            };
            let preferred = current();
            let holders = if self.sealed {
                source
                    .as_ref()
                    .map(|s| s.holders(&self.session))
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let inner = self
                .presentations
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if Arc::ptr_eq(&inner.sealed_generation, &generation) {
                break (inner, preferred, holders);
            }
        };
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
                identity: Arc::new(()),
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
        loop {
            let (owner, fanout, capabilities, locality, source, generation) = {
                let inner = self
                    .presentations
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some(owner) = inner
                    .owners
                    .get(id)
                    .filter(|owner| owner.session == self.session)
                else {
                    return false;
                };
                (
                    owner.snapshot(),
                    Arc::clone(&inner.fanout),
                    inner.sole_capabilities.clone(),
                    Arc::clone(&inner.locality),
                    inner.sealed_source.clone(),
                    Arc::clone(&inner.sealed_generation),
                )
            };
            let raiser_live = owner.raiser && {
                let fanout = fanout
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                PresentationInner::owner_live(&capabilities, locality.as_ref(), &fanout, &owner)
            };
            let available = raiser_live
                || owner
                    .sealed
                    .iter()
                    .any(|holder| source.as_ref().is_some_and(|s| s.current(holder)));
            let inner = self
                .presentations
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if Arc::ptr_eq(&inner.sealed_generation, &generation)
                && inner.sole_capabilities == capabilities
                && inner
                    .owners
                    .get(id)
                    .is_some_and(|current| current.matches_snapshot(&owner))
            {
                return available;
            }
        }
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
        let target = {
            let inner = self
                .presentations
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            inner
                .owners
                .get(id)
                .filter(|owner| owner.session == self.session && !owner.sealed.is_empty())
                .and_then(|owner| {
                    inner
                        .sealed_source
                        .clone()
                        .map(|source| (source, owner.sealed.clone()))
                })
        };
        if let Some((source, holders)) = target {
            source.settle(&holders, id, outcome, winner_grant);
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
        let target = {
            let inner = self
                .presentations
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            inner
                .owners
                .get(id)
                .filter(|owner| owner.session == self.session && !owner.sealed.is_empty())
                .and_then(|owner| {
                    inner
                        .sealed_source
                        .clone()
                        .map(|source| (source, owner.sealed.clone()))
                })
        };
        if let Some((source, holders)) = target {
            source.offer(
                &self.session,
                &holders,
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
        let (mut inner, sealed_live, holder_conns) = loop {
            let (owner, source, generation) = {
                let inner = self
                    .presentations
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                (
                    inner
                        .owners
                        .get(id)
                        .filter(|o| o.session == self.session)
                        .map(SecretOwner::snapshot),
                    inner.sealed_source.clone(),
                    Arc::clone(&inner.sealed_generation),
                )
            };
            let sealed_live = owner.as_ref().is_some_and(|owner| {
                owner
                    .sealed
                    .iter()
                    .any(|holder| source.as_ref().is_some_and(|s| s.current(holder)))
            });
            let holder_conns = owner
                .as_ref()
                .and_then(|owner| {
                    source.as_ref().map(|src| {
                        owner
                            .sealed
                            .iter()
                            .map(|holder| src.connection_of(holder))
                            .collect::<Vec<_>>()
                    })
                })
                .unwrap_or_default();
            let inner = self
                .presentations
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let current = inner.owners.get(id).filter(|o| o.session == self.session);
            let matches = match (&owner, current) {
                (Some(snapshot), Some(owner)) => owner.matches_snapshot(snapshot),
                (None, None) => true,
                _ => false,
            };
            if matches && Arc::ptr_eq(&inner.sealed_generation, &generation) {
                break (inner, sealed_live, holder_conns);
            }
        };
        let (cancel, line, conn, flush, notice_flush) = {
            let fanout_arc = inner.fanout.clone();
            let fanout = fanout_arc
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let flush = inner.flush.clone();

            // Notify non-presenters first.
            let live_raiser = inner
                .owners
                .get(id)
                .is_some_and(|o| o.session == self.session && o.raiser && inner.live(&fanout, o));
            let available = live_raiser || sealed_live;
            let notice_info = inner
                .owners
                .get(id)
                .filter(|o| o.session == self.session)
                .map(|o| {
                    let raising = if o.raised_by == NO_RAISING_CONNECTION {
                        None
                    } else {
                        Some(o.raised_by.clone())
                    };
                    (
                        available,
                        o.connection.clone(),
                        raising,
                        holder_conns.clone(),
                        o.raised_by.clone(),
                        o.origin.clone(),
                    )
                });

            let mut notice_flush = false;
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
                    notice_flush =
                        inner
                            .notices
                            .raised(&fanout, &self.session, id, &excluded, notice);
                } else if rec_type == Some("secret_input_settled") {
                    notice_flush = inner.notices.settled(
                        &fanout,
                        &self.session,
                        id,
                        json!({
                            "type": "secret_input_notice_settled",
                            "outcome": record.get("outcome"),
                        }),
                    );
                }
            }

            if !available {
                let cancel = inner.owners.get_mut(id).and_then(|o| o.cancel.take());
                (cancel, None, None, flush, notice_flush)
            } else {
                if !live_raiser {
                    (None, None, None, flush, notice_flush)
                } else {
                    record["sessionId"] = json!(self.session);
                    let line = serialize_json_line(&record).ok();
                    let conn = inner.owners.get(id).and_then(|o| o.connection.clone());
                    (None, line, conn, flush, notice_flush)
                }
            }
        };
        drop(inner);
        if notice_flush {
            flush();
        }
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
        let source = {
            let mut inner = self
                .presentations
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(owner) = inner.owners.get_mut(id) {
                owner.cancel = None;
            }
            inner.sealed_source.clone()
        };
        if let Some(source) = source {
            source.retire(id);
        }
    }

    fn forget(&self, id: &str) {
        let source = {
            let mut inner = self
                .presentations
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            inner.owners.remove(id);
            inner.sealed_source.clone()
        };
        if let Some(source) = source {
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

    struct ReentrantSource {
        state: std::sync::Weak<Mutex<PresentationInner>>,
        observations: Arc<Mutex<Vec<bool>>>,
    }

    impl SealedSecretSource for ReentrantSource {
        fn holders(&self, _: &str) -> Vec<SealedHolder> {
            let state = self.state.upgrade().expect("state");
            self.observations
                .lock()
                .expect("observations")
                .push(state.try_lock().is_ok());
            Vec::new()
        }
        fn current(&self, _: &SealedHolder) -> bool {
            let state = self.state.upgrade().expect("state");
            self.observations
                .lock()
                .expect("observations")
                .push(state.try_lock().is_ok());
            false
        }
        fn connection_of(&self, _: &SealedHolder) -> String {
            let state = self.state.upgrade().expect("state");
            self.observations
                .lock()
                .expect("observations")
                .push(state.try_lock().is_ok());
            String::new()
        }
        fn offer(&self, _: &str, _: &[SealedHolder], _: SealedSecretOffer) {
            let state = self.state.upgrade().expect("state");
            self.observations
                .lock()
                .expect("observations")
                .push(state.try_lock().is_ok());
        }
        fn withdraw(&self, _: &str) {
            let state = self.state.upgrade().expect("state");
            self.observations
                .lock()
                .expect("observations")
                .push(state.try_lock().is_ok());
        }
        fn retire(&self, _: &str) {
            let state = self.state.upgrade().expect("state");
            self.observations
                .lock()
                .expect("observations")
                .push(state.try_lock().is_ok());
        }
        fn on_change(&self, listener: Arc<dyn Fn() + Send + Sync>) -> Box<dyn FnOnce() + Send> {
            let state = self.state.upgrade().expect("state");
            let unlocked = state.try_lock().is_ok();
            self.observations
                .lock()
                .expect("observations")
                .push(unlocked);
            if unlocked {
                listener();
            }
            let state = self.state.clone();
            let observations = Arc::clone(&self.observations);
            Box::new(move || {
                let state = state.upgrade().expect("state");
                observations
                    .lock()
                    .expect("observations")
                    .push(state.try_lock().is_ok());
            })
        }
        fn eligible(&self, _: &SealedHolder) -> bool {
            false
        }
        fn principal_of(&self, _: &SealedHolder) -> Option<String> {
            None
        }
        fn answer<'a>(
            &'a self,
            _: &'a SealedHolder,
            _: &'a crate::secret_sealed_answers::SealedAnswerFrame,
        ) -> futures::future::BoxFuture<
            'a,
            Result<crate::secret_input_requests::SealedAnswer, SealedAnswerReply>,
        > {
            Box::pin(async { panic!("answer is not used by this test") })
        }
        fn settle(&self, _: &[SealedHolder], _: &str, _: SecretInputOutcome, _: Option<&str>) {
            let state = self.state.upgrade().expect("state");
            self.observations
                .lock()
                .expect("observations")
                .push(state.try_lock().is_ok());
        }
        fn admit_jti(&self, _: &str, _: i64) -> bool {
            false
        }
    }

    #[test]
    fn sealed_source_subscribe_and_unsubscribe_allow_reentry() {
        let pres = SecretInputPresentations::new(
            Arc::new(Mutex::new(SessionEventFanout::default())),
            Arc::new(|| None),
            Arc::new(|_| {}),
            Arc::new(|| {}),
            Arc::new(LocalOnlyLocality),
        );
        let observations = Arc::new(Mutex::new(Vec::new()));
        let source = Arc::new(ReentrantSource {
            state: Arc::downgrade(&pres.inner),
            observations: Arc::clone(&observations),
        });
        let unsubscribe = pres.use_sealed(Some(source));
        unsubscribe();
        assert_eq!(
            *observations.lock().expect("observations"),
            vec![true, true]
        );
    }

    #[test]
    fn sealed_source_retire_and_withdraw_allow_reentry() {
        let pres = SecretInputPresentations::new(
            Arc::new(Mutex::new(SessionEventFanout::default())),
            Arc::new(|| None),
            Arc::new(|_| {}),
            Arc::new(|| {}),
            Arc::new(LocalOnlyLocality),
        );
        let observations = Arc::new(Mutex::new(Vec::new()));
        let source = Arc::new(ReentrantSource {
            state: Arc::downgrade(&pres.inner),
            observations: Arc::clone(&observations),
        });
        let unsubscribe = pres.use_sealed(Some(source));
        observations.lock().expect("observations").clear();
        let route = pres.route("s1", false);
        route.settled("r1");
        route.forget("r1");
        assert_eq!(
            *observations.lock().expect("observations"),
            vec![true, true]
        );
        unsubscribe();
    }

    #[test]
    fn sealed_offer_and_outcome_allow_source_reentry() {
        let pres = SecretInputPresentations::new(
            Arc::new(Mutex::new(SessionEventFanout::default())),
            Arc::new(|| None),
            Arc::new(|_| {}),
            Arc::new(|| {}),
            Arc::new(LocalOnlyLocality),
        );
        let observations = Arc::new(Mutex::new(Vec::new()));
        let source = Arc::new(ReentrantSource {
            state: Arc::downgrade(&pres.inner),
            observations: Arc::clone(&observations),
        });
        let unsubscribe = pres.use_sealed(Some(source));
        pres.inner.lock().expect("state").owners.insert(
            "r1".into(),
            SecretOwner {
                identity: Arc::new(()),
                session: "s1".into(),
                connection: Some("c1".into()),
                cancel: None,
                raiser: false,
                sealed: vec![SealedHolder {
                    id: "holder".into(),
                    binding_id: 1,
                    durable_session_id: "s1".into(),
                    connection_id: "c1".into(),
                    principal_id: "p1".into(),
                }],
                background: false,
                raised_by: "agent".into(),
                origin: None,
            },
        );
        observations.lock().expect("observations").clear();
        let route = pres.route("s1", true);
        route.offer_sealed(
            "r1",
            SealedOfferDetails {
                hpke_pk: "key".into(),
                title: "title".into(),
                placeholder: None,
                deadline_at_ms: 1,
            },
        );
        route.sealed_outcome("r1", SecretInputOutcome::Cancelled, None);
        assert_eq!(
            *observations.lock().expect("observations"),
            vec![true, true]
        );
        observations.lock().expect("observations").clear();
        route.send(
            "r1",
            json!({"type":"secret_input_settled", "outcome":"cancelled"}),
        );
        let send_observations = observations.lock().expect("observations").clone();
        assert!(!send_observations.is_empty());
        assert!(send_observations.iter().all(|unlocked| *unlocked));
        observations.lock().expect("observations").clear();
        pres.detach("c1");
        assert_eq!(*observations.lock().expect("observations"), vec![true]);
        unsubscribe();
    }

    #[test]
    fn sealed_availability_allows_source_reentry() {
        let pres = SecretInputPresentations::new(
            Arc::new(Mutex::new(SessionEventFanout::default())),
            Arc::new(|| None),
            Arc::new(|_| {}),
            Arc::new(|| {}),
            Arc::new(LocalOnlyLocality),
        );
        let observations = Arc::new(Mutex::new(Vec::new()));
        let source = Arc::new(ReentrantSource {
            state: Arc::downgrade(&pres.inner),
            observations: Arc::clone(&observations),
        });
        let unsubscribe = pres.use_sealed(Some(source));
        pres.inner.lock().expect("state").owners.insert(
            "r1".into(),
            SecretOwner {
                identity: Arc::new(()),
                session: "s1".into(),
                connection: None,
                cancel: Some(Box::new(|| {})),
                raiser: false,
                sealed: vec![SealedHolder {
                    id: "holder".into(),
                    binding_id: 1,
                    durable_session_id: "s1".into(),
                    connection_id: "c1".into(),
                    principal_id: "p1".into(),
                }],
                background: false,
                raised_by: "agent".into(),
                origin: None,
            },
        );
        observations.lock().expect("observations").clear();
        assert!(!pres.route("s1", true).available("r1"));
        assert_eq!(*observations.lock().expect("observations"), vec![true]);
        observations.lock().expect("observations").clear();
        pres.recheck();
        assert_eq!(*observations.lock().expect("observations"), vec![true]);
        unsubscribe();
    }

    #[test]
    fn selecting_sealed_holders_allows_source_reentry() {
        let pres = SecretInputPresentations::new(
            Arc::new(Mutex::new(SessionEventFanout::default())),
            Arc::new(|| None),
            Arc::new(|_| {}),
            Arc::new(|| {}),
            Arc::new(LocalOnlyLocality),
        );
        let observations = Arc::new(Mutex::new(Vec::new()));
        let source = Arc::new(ReentrantSource {
            state: Arc::downgrade(&pres.inner),
            observations: Arc::clone(&observations),
        });
        let unsubscribe = pres.use_sealed(Some(source));
        observations.lock().expect("observations").clear();
        let _ = pres.route("s1", true).select("r1", Box::new(|| {}));
        assert_eq!(*observations.lock().expect("observations"), vec![true]);
        unsubscribe();
    }

    #[test]
    fn notice_flush_can_reenter_presentation_and_fanout() {
        let fanout = Arc::new(Mutex::new(SessionEventFanout::default()));
        {
            let mut fanout = fanout.lock().expect("fanout");
            for (id, capability) in [
                ("owner", SECRET_INPUT_CAPABILITY),
                ("watcher", RAISER_NOTICE_CAPABILITY),
            ] {
                fanout.targets.register(id);
                fanout.targets.set_capabilities(
                    id,
                    &[
                        capability.into(),
                        crate::custom_capability::INTERACTIVE_USER_CAPABILITY.into(),
                    ],
                );
                fanout.attach(id, "s1", 0.0).expect("attach");
            }
        }
        let slot = Arc::new(Mutex::new(
            std::sync::Weak::<Mutex<PresentationInner>>::new(),
        ));
        let observations = Arc::new(Mutex::new(Vec::new()));
        let callback_slot = Arc::clone(&slot);
        let callback_fanout = Arc::clone(&fanout);
        let callback_observations = Arc::clone(&observations);
        let pres = SecretInputPresentations::new(
            fanout,
            Arc::new(|| None),
            Arc::new(|_| {}),
            Arc::new(move || {
                let state = callback_slot
                    .lock()
                    .expect("slot")
                    .upgrade()
                    .expect("state");
                let unlocked = state.try_lock().is_ok() && callback_fanout.try_lock().is_ok();
                callback_observations
                    .lock()
                    .expect("observations")
                    .push(unlocked);
            }),
            Arc::new(LocalOnlyLocality),
        );
        *slot.lock().expect("slot") = Arc::downgrade(&pres.inner);
        pres.inner.lock().expect("state").owners.insert(
            "r1".into(),
            SecretOwner {
                identity: Arc::new(()),
                session: "s1".into(),
                connection: Some("owner".into()),
                cancel: None,
                raiser: true,
                sealed: Vec::new(),
                background: false,
                raised_by: "agent".into(),
                origin: None,
            },
        );
        let route = pres.route("s1", false);
        route.send(
            "r1",
            json!({"type":"extension_ui_request", "title":"prompt"}),
        );
        route.send(
            "r1",
            json!({"type":"secret_input_settled", "outcome":"cancelled"}),
        );
        let observations = observations.lock().expect("observations");
        assert!(observations.len() >= 2);
        assert!(observations.iter().all(|unlocked| *unlocked));
    }

    #[test]
    fn stale_sealed_unsubscribe_keeps_new_registration() {
        let pres = SecretInputPresentations::new(
            Arc::new(Mutex::new(SessionEventFanout::default())),
            Arc::new(|| None),
            Arc::new(|_| {}),
            Arc::new(|| {}),
            Arc::new(LocalOnlyLocality),
        );
        let source = Arc::new(ReentrantSource {
            state: Arc::downgrade(&pres.inner),
            observations: Arc::new(Mutex::new(Vec::new())),
        });
        let first = pres.use_sealed(Some(source.clone()));
        let second = pres.use_sealed(Some(source));
        first();
        assert!(pres.inner.lock().expect("state").sealed_source.is_some());
        second();
        assert!(pres.inner.lock().expect("state").sealed_source.is_none());
    }

    #[test]
    fn cancellation_callback_runs_without_presentation_lock() {
        let fanout = Arc::new(Mutex::new(SessionEventFanout::default()));
        let pres = SecretInputPresentations::new(
            fanout,
            Arc::new(|| None),
            Arc::new(|_| {}),
            Arc::new(|| {}),
            Arc::new(LocalOnlyLocality),
        );
        let route = pres.route("s1", false);
        route.handshake(&[
            SECRET_INPUT_CAPABILITY.into(),
            crate::custom_capability::INTERACTIVE_USER_CAPABILITY.into(),
        ]);
        let unlocked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = Arc::clone(&unlocked);
        let state = Arc::downgrade(&pres.inner);
        assert_eq!(
            route.select(
                "r1",
                Box::new(move || {
                    let state = state.upgrade().expect("presentation state");
                    observed.store(
                        state.try_lock().is_ok(),
                        std::sync::atomic::Ordering::SeqCst,
                    );
                }),
            ),
            None
        );
        pres.handshake(&[]);
        assert!(unlocked.load(std::sync::atomic::Ordering::SeqCst));
    }

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
