//! senpi `modes/rpc/compact-recovery-requests.ts`: the page requests of one host over its
//! checkpoint owner and event leases (S11b, RULING-62 Q1 A).

use crate::compact_event_leases::EventLeases;
use crate::compact_recovery_bytes::{CancelSignal, RecoveryPage};
use crate::compact_recovery_cursor::{CursorBinding, RecoveryCursors};
use crate::compact_recovery_ids::parse_bounded_id;
use crate::compact_recovery_state::RecoveryCheckpoints;
use crate::compact_recovery_types::{
    RecoveryCaller, RecoveryFailure, RecoveryOutcome, RepairReason, repair_needed,
};
use base64::Engine;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

/// At most this many page requests are outstanding on one socket across all of its tokens (R4.B3).
pub const SOCKET_PAGE_REQUESTS_MAX: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryPageRequest {
    pub request_id: String,
    pub admission_id: String,
    pub token: Option<String>,
    pub lease: Option<(String, String)>,
    pub component_ref: String,
    pub cursor: Option<String>,
}

impl RecoveryPageRequest {
    fn kind_and_id(&self) -> (&str, Option<&str>) {
        match &self.lease {
            Some((kind, id)) => (kind.as_str(), Some(id.as_str())),
            None => ("checkpoint", self.token.as_deref()),
        }
    }

    fn same_request(&self, other: &Self) -> bool {
        self.request_id == other.request_id
            && self.component_ref == other.component_ref
            && self.cursor == other.cursor
            && self.kind_and_id().1 == other.kind_and_id().1
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryPageAnswer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair: Option<Value>,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<crate::compact_event_leases::LeaseRefIdentity>,
    pub component_ref: String,
    pub ordinal: u64,
    pub offset: usize,
    pub length: usize,
    pub digest: String,
    pub bytes: String,
    pub cursor: Option<String>,
    pub complete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestRefusalReason {
    MalformedRequest,
    Busy,
    RequestLimit,
    RequestMismatch,
    InvalidCursor,
    CursorOutOfRange,
    OrdinalExhausted,
    LeaseKindUnsupported,
}

impl RequestRefusalReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MalformedRequest => "malformed_request",
            Self::Busy => "busy",
            Self::RequestLimit => "request_limit",
            Self::RequestMismatch => "request_mismatch",
            Self::InvalidCursor => "invalid_cursor",
            Self::CursorOutOfRange => "cursor_out_of_range",
            Self::OrdinalExhausted => "ordinal_exhausted",
            Self::LeaseKindUnsupported => "lease_kind_unsupported",
        }
    }

    pub const fn hint(self) -> RequestRefusalHint {
        match self {
            Self::MalformedRequest | Self::RequestMismatch | Self::LeaseKindUnsupported => {
                RequestRefusalHint::NewRequest
            }
            Self::Busy | Self::RequestLimit => RequestRefusalHint::Retry,
            Self::InvalidCursor | Self::CursorOutOfRange => RequestRefusalHint::RestartFromCatalog,
            Self::OrdinalExhausted => RequestRefusalHint::ReleaseToken,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestRefusalHint {
    Retry,
    RestartFromCatalog,
    ReleaseToken,
    NewRequest,
}

impl RequestRefusalHint {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::RestartFromCatalog => "restart_from_catalog",
            Self::ReleaseToken => "release_token",
            Self::NewRequest => "new_request",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestRefusal {
    pub kind: String,
    pub reason: RequestRefusalReason,
    pub hint: RequestRefusalHint,
}

impl RequestRefusal {
    pub fn new(reason: RequestRefusalReason) -> Self {
        Self {
            kind: "request_refused".into(),
            reason,
            hint: reason.hint(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryPageFailure {
    Recovery(RecoveryFailure),
    RequestRefused(RequestRefusal),
}

impl From<RecoveryFailure> for RecoveryPageFailure {
    fn from(f: RecoveryFailure) -> Self {
        Self::Recovery(f)
    }
}

pub type RecoveryPageOutcome = Result<RecoveryPageAnswer, RecoveryPageFailure>;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryPageCounters {
    pub tokens: usize,
    pub outstanding: usize,
    pub cached_bytes: usize,
    pub refused: BTreeMap<RequestRefusalReason, u64>,
}

struct TokenRequests {
    active: bool,
    ordinal: u64,
    current: Option<(RecoveryPageRequest, RecoveryPageAnswer)>,
}

#[allow(clippy::large_enum_variant)]
enum SourceKind {
    Checkpoint {
        token: crate::compact_recovery_types::CheckpointToken,
        generation: u64,
    },
    Event {
        held: Arc<crate::compact_event_leases::HeldLease>,
    },
}

struct PageSource {
    id: String,
    generation: u64,
    kind: SourceKind,
}

struct RequestsState {
    tokens: HashMap<String, TokenRequests>,
    outstanding: HashMap<String, usize>,
    refusals: BTreeMap<RequestRefusalReason, u64>,
    lifetimes: HashMap<String, CancelSignal>,
}

/// The page requests of one host over its checkpoint owner and event leases (S11b, RULING-62 Q1 A).
pub struct RecoveryPageRequests {
    checkpoints: RecoveryCheckpoints,
    cursors: RecoveryCursors,
    events: Option<EventLeases>,
    state: Arc<Mutex<RequestsState>>,
}

impl RecoveryPageRequests {
    pub fn new(
        checkpoints: RecoveryCheckpoints,
        cursors: Option<RecoveryCursors>,
        events: Option<EventLeases>,
    ) -> Self {
        let state = Arc::new(Mutex::new(RequestsState {
            tokens: HashMap::new(),
            outstanding: HashMap::new(),
            refusals: BTreeMap::new(),
            lifetimes: HashMap::new(),
        }));
        {
            let st = state.clone();
            checkpoints.on_token_end(move |token_id| {
                st.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .tokens
                    .remove(token_id);
            });
        }
        if let Some(ev) = &events {
            let st = state.clone();
            ev.on_lease_end(move |lease_id| {
                st.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .tokens
                    .remove(lease_id);
            });
        }
        Self {
            checkpoints,
            cursors: cursors.unwrap_or_default(),
            events,
            state,
        }
    }

    pub fn status(&self) -> RecoveryPageCounters {
        let st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let outstanding = st.outstanding.values().sum();
        let cached_bytes = st
            .tokens
            .values()
            .filter_map(|t| t.current.as_ref().map(|(_, ans)| ans.bytes.len()))
            .sum();
        RecoveryPageCounters {
            tokens: st.tokens.len(),
            outstanding,
            cached_bytes,
            refused: st.refusals.clone(),
        }
    }

    pub fn forget_peer(&self, socket_epoch: &str) {
        let sig = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .lifetimes
            .remove(socket_epoch);
        if let Some(sig) = sig {
            sig.abort();
        }
    }

    pub async fn request(
        &self,
        caller: &RecoveryCaller,
        request: &RecoveryPageRequest,
        signal: Option<&CancelSignal>,
    ) -> RecoveryPageOutcome {
        let (kind, id_opt) = request.kind_and_id();
        let valid_kind = kind == "event" || kind == "checkpoint";
        let Some(id) = id_opt else {
            return self.refuse(RequestRefusalReason::MalformedRequest);
        };
        if !valid_kind
            || parse_bounded_id(&request.request_id).is_err()
            || parse_bounded_id(&request.admission_id).is_err()
            || parse_bounded_id(id).is_err()
            || parse_bounded_id(&request.component_ref).is_err()
        {
            return self.refuse(RequestRefusalReason::MalformedRequest);
        }

        {
            let st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if st
                .outstanding
                .get(&caller.socket_epoch)
                .copied()
                .unwrap_or(0)
                >= SOCKET_PAGE_REQUESTS_MAX
            {
                return self.refuse(RequestRefusalReason::RequestLimit);
            }
        }

        let source = if kind == "event" {
            self.event_source(caller, id, &request.admission_id)?
        } else {
            self.checkpoint_source(caller, id, &request.admission_id)?
        };

        let binding = CursorBinding {
            socket_epoch: caller.socket_epoch.clone(),
            session_id: caller.session_id.clone(),
            token_id: source.id.clone(),
            generation: source.generation,
            component_ref: request.component_ref.clone(),
        };

        let position = match &request.cursor {
            Some(c) => {
                let Some(pos) = self.cursors.open(c, &binding) else {
                    return self.refuse(RequestRefusalReason::InvalidCursor);
                };
                Some(pos)
            }
            None => None,
        };

        enum LookupOutcome {
            Cached(Box<RecoveryPageAnswer>),
            Refuse(RequestRefusalReason),
            Ready(CancelSignal),
        }
        let outcome = {
            let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let entry = st.tokens.entry(source.id.clone()).or_insert(TokenRequests {
                active: false,
                ordinal: 0,
                current: None,
            });
            if entry.active {
                LookupOutcome::Refuse(RequestRefusalReason::Busy)
            } else if let Some((prev_req, prev_ans)) = &entry.current
                && prev_req.request_id == request.request_id
            {
                if prev_req.same_request(request) {
                    LookupOutcome::Cached(Box::new(prev_ans.clone()))
                } else {
                    LookupOutcome::Refuse(RequestRefusalReason::RequestMismatch)
                }
            } else {
                let last = position.map_or(entry.ordinal, |p| p.ordinal);
                if last == u64::MAX {
                    LookupOutcome::Refuse(RequestRefusalReason::OrdinalExhausted)
                } else if let Some(pos) = position
                    && pos.ordinal != entry.ordinal
                {
                    LookupOutcome::Refuse(RequestRefusalReason::CursorOutOfRange)
                } else {
                    entry.active = true;
                    *st.outstanding
                        .entry(caller.socket_epoch.clone())
                        .or_insert(0) += 1;
                    let sig = st
                        .lifetimes
                        .entry(caller.socket_epoch.clone())
                        .or_default()
                        .clone();
                    LookupOutcome::Ready(sig)
                }
            }
        };
        let lifetime = match outcome {
            LookupOutcome::Cached(ans) => return Ok(*ans),
            LookupOutcome::Refuse(reason) => return self.refuse(reason),
            LookupOutcome::Ready(sig) => sig,
        };

        let offset = position.map_or(0, |p| p.offset);
        let budget = if signal.is_some_and(CancelSignal::is_aborted) || lifetime.is_aborted() {
            let s = CancelSignal::new();
            s.abort();
            s
        } else {
            lifetime.clone()
        };

        let page_fut: BoxFuture<'_, RecoveryOutcome<RecoveryPage>> = match &source.kind {
            SourceKind::Checkpoint { token, .. } => {
                let checkpoints = self.checkpoints.clone();
                let tok = token.clone();
                let cal = caller.clone();
                let cref = request.component_ref.clone();
                Box::pin(async move {
                    checkpoints
                        .read_catalog_page(&tok, &cal, &cref, offset, Some(&budget))
                        .await
                })
            }
            SourceKind::Event { held } => {
                let events = self.events.clone();
                let held = held.clone();
                let cref = request.component_ref.clone();
                Box::pin(async move {
                    let Some(events) = events else {
                        return Err(repair_needed(RepairReason::UnknownToken));
                    };
                    events.page(&held, &cref, offset, Some(&budget)).await
                })
            }
        };

        let page_res = page_fut.await;

        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let left = st
            .outstanding
            .get(&caller.socket_epoch)
            .copied()
            .unwrap_or(1)
            .saturating_sub(1);
        if left > 0 {
            st.outstanding.insert(caller.socket_epoch.clone(), left);
        } else {
            st.outstanding.remove(&caller.socket_epoch);
            st.lifetimes.remove(&caller.socket_epoch);
        }

        let page = match page_res {
            Ok(page) => page,
            Err(failure) => {
                st.tokens.remove(&source.id);
                return Err(failure.into());
            }
        };

        let Some(token_state) = st.tokens.get_mut(&source.id) else {
            return Err(repair_needed(RepairReason::Released).into());
        };
        token_state.active = false;
        let ordinal = token_state.ordinal + 1;
        let cursor = page.next.map(|next_offset| {
            self.cursors.mint(
                &binding,
                crate::compact_recovery_cursor::CursorPosition {
                    ordinal,
                    offset: next_offset,
                },
            )
        });
        let encoded_bytes = base64::engine::general_purpose::STANDARD.encode(&page.bytes);
        let (token_field, gen_field, lease_field) = match &source.kind {
            SourceKind::Checkpoint { token, generation } => {
                (Some(token.id.clone()), Some(*generation), None)
            }
            SourceKind::Event { held } => (
                None,
                None,
                Some(crate::compact_event_leases::LeaseRefIdentity {
                    kind: "event".into(),
                    id: held.lease.id.clone(),
                }),
            ),
        };
        let answer = RecoveryPageAnswer {
            repair: None,
            request_id: request.request_id.clone(),
            token: token_field,
            generation: gen_field,
            lease: lease_field,
            component_ref: request.component_ref.clone(),
            ordinal,
            offset: page.offset,
            length: page.length,
            digest: page.digest.into_inner(),
            bytes: encoded_bytes,
            cursor,
            complete: page.complete,
        };
        token_state.ordinal = ordinal;
        token_state.current = Some((request.clone(), answer.clone()));
        Ok(answer)
    }

    fn checkpoint_source(
        &self,
        caller: &RecoveryCaller,
        token_id: &str,
        admission_id: &str,
    ) -> Result<PageSource, RecoveryPageFailure> {
        let token = self
            .checkpoints
            .tokens()
            .into_iter()
            .find(|t| t.id == token_id);
        let stream_has_adm = self
            .checkpoints
            .stream(caller)
            .is_some_and(|s| s.routes.iter().any(|r| r == admission_id));
        let Some(token) = token else {
            return Err(repair_needed(RepairReason::UnknownToken).into());
        };
        if token.admission.socket_epoch != caller.socket_epoch
            || token.admission.session_id != caller.session_id
            || !stream_has_adm
        {
            return Err(repair_needed(RepairReason::UnknownToken).into());
        }
        let claimed = self.checkpoints.check(&token, caller)?;
        let generation = claimed.admission.generation;
        Ok(PageSource {
            id: claimed.id.clone(),
            generation,
            kind: SourceKind::Checkpoint {
                token: claimed,
                generation,
            },
        })
    }

    fn event_source(
        &self,
        caller: &RecoveryCaller,
        lease_id: &str,
        admission_id: &str,
    ) -> Result<PageSource, RecoveryPageFailure> {
        let Some(events) = &self.events else {
            return Err(repair_needed(RepairReason::UnknownToken).into());
        };
        let held = events.claim(caller, lease_id, admission_id)?;
        Ok(PageSource {
            id: held.lease.id.clone(),
            generation: 0,
            kind: SourceKind::Event { held },
        })
    }

    fn refuse<T>(&self, reason: RequestRefusalReason) -> Result<T, RecoveryPageFailure> {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        *st.refusals.entry(reason).or_insert(0) += 1;
        Err(RecoveryPageFailure::RequestRefused(RequestRefusal::new(
            reason,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_record_staging::RecordContent;
    use serde_json::json;

    #[tokio::test]
    async fn serves_event_lease_pages_handles_exact_retry_and_refuses_mismatch() {
        let checkpoints = RecoveryCheckpoints::default();
        let events = EventLeases::default();
        let requests = RecoveryPageRequests::new(checkpoints, None, Some(events.clone()));

        let record = json!({"type":"message_end","id":"m1"});
        let text = serde_json::to_string(&record).expect("json");
        let lease = events
            .mint(
                "se-1",
                "s1",
                "adm-1",
                "ep-1",
                1,
                &record,
                RecordContent::Text(text),
                None,
            )
            .expect("mint");
        let caller = RecoveryCaller {
            socket_epoch: "se-1".into(),
            session_id: "s1".into(),
        };
        let req = RecoveryPageRequest {
            request_id: "req-1".into(),
            admission_id: "adm-1".into(),
            token: None,
            lease: Some(("event".into(), lease.id.clone())),
            component_ref: "record".into(),
            cursor: None,
        };
        let ans1 = requests.request(&caller, &req, None).await.expect("page 1");
        assert_eq!(ans1.ordinal, 1);
        assert!(ans1.complete);

        // Exact retry returns identical cached answer
        let ans2 = requests.request(&caller, &req, None).await.expect("retry");
        assert_eq!(ans1, ans2);

        // Mismatch with same request_id
        let mut bad_req = req.clone();
        bad_req.component_ref = "manifest".into();
        let err = requests
            .request(&caller, &bad_req, None)
            .await
            .expect_err("mismatch");
        assert!(matches!(
            err,
            RecoveryPageFailure::RequestRefused(RequestRefusal {
                reason: RequestRefusalReason::RequestMismatch,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn rejects_malformed_ids_and_invalid_cursors() {
        let requests = RecoveryPageRequests::new(RecoveryCheckpoints::default(), None, None);
        let caller = RecoveryCaller {
            socket_epoch: "se-1".into(),
            session_id: "s1".into(),
        };
        let bad = RecoveryPageRequest {
            request_id: "bad/id".into(),
            admission_id: "adm-1".into(),
            token: Some("tok-1".into()),
            lease: None,
            component_ref: "manifest".into(),
            cursor: None,
        };
        let err = requests
            .request(&caller, &bad, None)
            .await
            .expect_err("malformed");
        assert!(matches!(
            err,
            RecoveryPageFailure::RequestRefused(RequestRefusal {
                reason: RequestRefusalReason::MalformedRequest,
                ..
            })
        ));
    }
}
