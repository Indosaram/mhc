//! Port of senpi `packages/coding-agent/src/modes/rpc/secret-input-requests.ts`.
//!
//! Private, live-only secret input request state. Never registered in progress, snapshot,
//! transcript, or answer-ledger maps.

use crate::extension_ui_response::answered_ui_request_id;
use crate::secret_sealed::{SecretInputOutcome, generate_secret_request_key};
use crate::secret_sealed_answers::open_sealed_value;
use crate::secret_sealed_hpke::{StubX25519Dh, X25519Dh};
use futures::future::BoxFuture;
use maho_ai::utils::abort::AbortSignal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretInputUnavailableReason {
    Headless,
    ClientUnsupported,
    NoPresenter,
}

impl SecretInputUnavailableReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Headless => "headless",
            Self::ClientUnsupported => "client_unsupported",
            Self::NoPresenter => "no_presenter",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "headless" => Some(Self::Headless),
            "client_unsupported" => Some(Self::ClientUnsupported),
            "no_presenter" => Some(Self::NoPresenter),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("This client cannot collect a secret; use the TUI or the host's settings route.")]
pub struct SecretInputUnavailableError {
    pub reason: SecretInputUnavailableReason,
}

impl SecretInputUnavailableError {
    pub const CODE: &'static str = "secret_input_unavailable";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SealedAnswerValue {
    pub jti: String,
    pub exp: i64,
    pub enc: String,
    pub ct: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SealedAnswer {
    pub ui_request_id: String,
    pub principal: String,
    pub grant_id: String,
    pub answer_id: String,
    pub req: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<SealedAnswerValue>,
}

pub trait SealedClaim: Send + Sync {
    fn current(&self) -> bool;
    fn admit_jti(&self, jti: &str, exp: i64) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SealedAnswerDisposition {
    Applied,
    Replay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SealedAnswerError {
    AlreadyAnswered,
    UnknownExtensionUiRequest,
    SecretStepupRefused,
    InvalidSecretResponse,
    SecretAnswerRefused,
}

impl SealedAnswerError {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyAnswered => "already_answered",
            Self::UnknownExtensionUiRequest => "unknown_extension_ui_request",
            Self::SecretStepupRefused => "secret_stepup_refused",
            Self::InvalidSecretResponse => "invalid_secret_response",
            Self::SecretAnswerRefused => "secret_answer_refused",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "already_answered" => Some(Self::AlreadyAnswered),
            "unknown_extension_ui_request" => Some(Self::UnknownExtensionUiRequest),
            "secret_stepup_refused" => Some(Self::SecretStepupRefused),
            "invalid_secret_response" => Some(Self::InvalidSecretResponse),
            "secret_answer_refused" => Some(Self::SecretAnswerRefused),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealedAnswerReply {
    Disposition(SealedAnswerDisposition),
    Error(SealedAnswerError),
}

impl SealedAnswerReply {
    #[allow(non_upper_case_globals)]
    pub const Applied: Self = Self::Disposition(SealedAnswerDisposition::Applied);
    #[allow(non_upper_case_globals)]
    pub const Replay: Self = Self::Disposition(SealedAnswerDisposition::Replay);

    pub fn to_wire(&self) -> Value {
        match self {
            Self::Disposition(SealedAnswerDisposition::Applied) => {
                json!({ "disposition": "applied" })
            }
            Self::Disposition(SealedAnswerDisposition::Replay) => {
                json!({ "disposition": "replay" })
            }
            Self::Error(err) => json!({ "error": err.as_str() }),
        }
    }

    pub fn from_wire(value: &Value) -> Option<Self> {
        let obj = value.as_object()?;
        match obj.get("disposition").and_then(Value::as_str) {
            Some("applied") => return Some(Self::Applied),
            Some("replay") => return Some(Self::Replay),
            _ => {}
        }
        obj.get("error")
            .and_then(Value::as_str)
            .and_then(SealedAnswerError::parse)
            .map(Self::Error)
    }
}

pub trait SealedLatch: Send + Sync {
    fn settled(
        &self,
        ui_request_id: &str,
        answer_id: &str,
        principal: &str,
    ) -> Option<SealedAnswerReply>;
    fn respond<'a>(
        &'a self,
        answer: SealedAnswer,
        claim: &'a dyn SealedClaim,
    ) -> BoxFuture<'a, SealedAnswerReply>;
}

#[derive(Debug, Clone)]
pub struct SealedOfferDetails {
    pub hpke_pk: String,
    pub title: String,
    pub placeholder: Option<String>,
    pub deadline_at_ms: i64,
}

pub trait SecretInputRoute: Send + Sync {
    fn select(
        &self,
        id: &str,
        unavailable: Box<dyn FnOnce() + Send + Sync>,
    ) -> Option<SecretInputUnavailableReason>;
    fn accepts(&self, id: &str) -> bool;
    fn available(&self, id: &str) -> bool;
    fn presenter(&self, id: &str) -> Option<String>;
    fn send(&self, id: &str, record: Value);
    fn settled(&self, id: &str);
    fn forget(&self, id: &str);
    fn handshake(&self, _capabilities: &[String]) {}
    fn sealed(&self, _id: &str) -> bool {
        false
    }
    fn offer_sealed(&self, _id: &str, _offer: SealedOfferDetails) {}
    fn sealed_outcome(&self, _id: &str, _outcome: SecretInputOutcome, _winner_grant: Option<&str>) {
    }
    fn raiser(&self, _id: &str) -> bool {
        true
    }
    fn bind_sealed_responder(&self, _latch: Option<Arc<dyn SealedLatch>>) {}
}

pub struct SecretInputOptions {
    pub timeout_ms: Option<u64>,
    pub signal: Option<AbortSignal>,
    pub tool_call_id: Option<String>,
    pub parent_tool_call_id: Option<String>,
}

/// Validates a secret value (`isSecretInputValue`): refuses control characters and line/paragraph separators.
pub fn is_secret_input_value(value: &str) -> bool {
    !value.chars().any(|c| {
        matches!(
            c,
            '\u{0000}'..='\u{001f}' | '\u{007f}'..='\u{009f}' | '\u{2028}' | '\u{2029}'
        )
    })
}

pub fn resolve_secret_input_timeout(timeout_ms: Option<u64>) -> Result<u64, &'static str> {
    let requested = timeout_ms.unwrap_or(30 * 60 * 1000);
    if requested == 0 {
        return Err("Invalid secret input timeout");
    }
    const MAX_SAFE_INT: i64 = (1i64 << 53) - 1;
    let now = now_ms();
    if now.saturating_add(requested as i64) > MAX_SAFE_INT {
        return Err("Invalid secret input timeout");
    }
    Ok(requested)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Receipt {
    answer_id: String,
    principal: Option<String>,
}

struct Pending {
    deadline_ms: i64,
    key: Option<[u8; 32]>,
    hpke_pk: Option<String>,
    sender: Option<tokio::sync::oneshot::Sender<Option<String>>>,
    deadline_task: Option<tokio::task::JoinHandle<()>>,
}

struct State {
    route: Arc<dyn SecretInputRoute>,
    admit: Arc<dyn Fn() + Send + Sync>,
    dh: Arc<dyn X25519Dh>,
    pending: HashMap<String, Pending>,
    receipts: HashMap<String, Option<Receipt>>,
    receipt_order: VecDeque<String>,
}

impl State {
    fn remember(&mut self, id: &str) {
        if !self.receipts.contains_key(id) {
            self.receipt_order.push_back(id.to_owned());
        }
        self.receipts.insert(id.to_owned(), None);
        while self.receipts.len() > 1024 {
            if let Some(oldest) = self.receipt_order.pop_front() {
                self.receipts.remove(&oldest);
                self.route.forget(&oldest);
            }
        }
    }

    fn finish(
        &mut self,
        id: &str,
        outcome: SecretInputOutcome,
        value: Option<String>,
        winner_grant: Option<&str>,
    ) -> bool {
        let Some(mut pending) = self.pending.remove(id) else {
            return false;
        };
        if let Some(task) = pending.deadline_task.take() {
            task.abort();
        }
        if let Some(mut key) = pending.key.take() {
            key.fill(0);
        }
        self.remember(id);
        self.route.send(
            id,
            json!({ "type": "secret_input_settled", "id": id, "outcome": outcome.as_str() }),
        );
        self.route.sealed_outcome(id, outcome, winner_grant);
        self.route.settled(id);
        if let Some(sender) = pending.sender.take() {
            let _ = sender.send(value);
        }
        true
    }

    fn receipt(&self, id: &str, answer_id: &str, principal: &str) -> SealedAnswerReply {
        let Some(Some(receipt)) = self.receipts.get(id) else {
            return SealedAnswerReply::Error(SealedAnswerError::UnknownExtensionUiRequest);
        };
        if receipt.answer_id == answer_id && receipt.principal.as_deref() == Some(principal) {
            SealedAnswerReply::Replay
        } else {
            SealedAnswerReply::Error(SealedAnswerError::AlreadyAnswered)
        }
    }
}

struct RequestsLatch {
    state: Weak<Mutex<State>>,
}

impl SealedLatch for RequestsLatch {
    fn settled(
        &self,
        ui_request_id: &str,
        answer_id: &str,
        principal: &str,
    ) -> Option<SealedAnswerReply> {
        let state = self.state.upgrade()?;
        let guard = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.pending.contains_key(ui_request_id) {
            None
        } else {
            Some(guard.receipt(ui_request_id, answer_id, principal))
        }
    }

    fn respond<'a>(
        &'a self,
        answer: SealedAnswer,
        claim: &'a dyn SealedClaim,
    ) -> BoxFuture<'a, SealedAnswerReply> {
        Box::pin(async move {
            let Some(state) = self.state.upgrade() else {
                return SealedAnswerReply::Error(SealedAnswerError::UnknownExtensionUiRequest);
            };
            let mut guard = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let id = answer.ui_request_id.clone();
            let Some(pending) = guard.pending.get(&id) else {
                return guard.receipt(&id, &answer.answer_id, &answer.principal);
            };
            let deadline_ms = pending.deadline_ms;
            let key = pending.key;
            let hpke_pk = pending.hpke_pk.clone();
            if let Some(val) = &answer.value
                && !claim.admit_jti(&val.jti, val.exp)
            {
                return SealedAnswerReply::Error(SealedAnswerError::SecretStepupRefused);
            }
            if now_ms() >= deadline_ms {
                guard.finish(&id, SecretInputOutcome::TimedOut, None, None);
                return SealedAnswerReply::Error(SealedAnswerError::UnknownExtensionUiRequest);
            }
            if !claim.current() {
                return SealedAnswerReply::Error(SealedAnswerError::SecretAnswerRefused);
            }
            let opened_value = if answer.value.is_some() {
                let (Some(key), Some(hpke_pk)) = (key, hpke_pk.as_deref()) else {
                    return SealedAnswerReply::Error(SealedAnswerError::InvalidSecretResponse);
                };
                let opened = open_sealed_value(&answer, &key, hpke_pk, guard.dh.as_ref());
                match opened {
                    Some(v) if is_secret_input_value(&v) => Some(v),
                    _ => {
                        return SealedAnswerReply::Error(SealedAnswerError::InvalidSecretResponse);
                    }
                }
            } else {
                None
            };
            let outcome = if answer.value.is_some() {
                SecretInputOutcome::Answered
            } else {
                SecretInputOutcome::Cancelled
            };
            (guard.admit)();
            let winner = answer.grant_id.clone();
            guard.finish(&id, outcome, opened_value, Some(&winner));
            guard.receipts.insert(
                id,
                Some(Receipt {
                    answer_id: answer.answer_id,
                    principal: Some(answer.principal),
                }),
            );
            SealedAnswerReply::Applied
        })
    }
}

#[derive(Clone)]
pub struct SecretInputRequests {
    state: Arc<Mutex<State>>,
}

impl SecretInputRequests {
    pub fn new(route: Arc<dyn SecretInputRoute>, admit: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self::with_dh(route, admit, Arc::new(StubX25519Dh))
    }

    pub fn with_dh(
        route: Arc<dyn SecretInputRoute>,
        admit: Arc<dyn Fn() + Send + Sync>,
        dh: Arc<dyn X25519Dh>,
    ) -> Self {
        let state = Arc::new(Mutex::new(State {
            route: route.clone(),
            admit,
            dh,
            pending: HashMap::new(),
            receipts: HashMap::new(),
            receipt_order: VecDeque::new(),
        }));
        let latch: Arc<dyn SealedLatch> = Arc::new(RequestsLatch {
            state: Arc::downgrade(&state),
        });
        route.bind_sealed_responder(Some(latch));
        Self { state }
    }

    pub async fn input(
        &self,
        title: &str,
        placeholder: Option<&str>,
        opts: Option<SecretInputOptions>,
        created: Option<&(dyn Fn(&str) + Send + Sync)>,
    ) -> Result<Option<String>, anyhow::Error> {
        let opts = opts.unwrap_or(SecretInputOptions {
            timeout_ms: None,
            signal: None,
            tool_call_id: None,
            parent_tool_call_id: None,
        });
        if opts.signal.as_ref().is_some_and(AbortSignal::aborted) {
            return Ok(None);
        }
        let timeout_ms = resolve_secret_input_timeout(opts.timeout_ms)
            .map_err(|msg| anyhow::anyhow!("{msg}"))?;
        let id = uuid::Uuid::new_v4().to_string();
        let route = {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .route
                .clone()
        };
        let state_for_unavail = self.state.clone();
        let id_for_unavail = id.clone();
        if let Some(reason) = route.select(
            &id,
            Box::new(move || {
                state_for_unavail
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .finish(&id_for_unavail, SecretInputOutcome::Unavailable, None, None);
            }),
        ) {
            return Err(SecretInputUnavailableError { reason }.into());
        }

        let deadline_ms = now_ms() + timeout_ms as i64;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let sealed = route.sealed(&id);

        let deadline_task = tokio::runtime::Handle::try_current().ok().map(|handle| {
            let state_weak = Arc::downgrade(&self.state);
            let id_timeout = id.clone();
            handle.spawn(async move {
                tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
                if let Some(state) = state_weak.upgrade() {
                    state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .finish(&id_timeout, SecretInputOutcome::TimedOut, None, None);
                }
            })
        });

        {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.pending.insert(
                id.clone(),
                Pending {
                    deadline_ms,
                    key: None,
                    hpke_pk: None,
                    sender: Some(tx),
                    deadline_task,
                },
            );
        }

        if let Some(cb) = created {
            cb(&id);
        }

        if let Some(signal) = opts.signal.clone() {
            if signal.aborted() {
                self.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .finish(&id, SecretInputOutcome::Cancelled, None, None);
                return Ok(rx.await.ok().flatten());
            }
            let state_weak = Arc::downgrade(&self.state);
            let id_abort = id.clone();
            tokio::spawn(async move {
                signal.cancelled().await;
                if let Some(state) = state_weak.upgrade() {
                    state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .finish(&id_abort, SecretInputOutcome::Cancelled, None, None);
                }
            });
        }

        let mut record = json!({
            "type": "extension_ui_request",
            "id": id,
            "method": "input",
            "secret": true,
            "title": title,
            "timeout": timeout_ms,
            "deadlineAtMs": deadline_ms,
        });
        if let Some(p) = placeholder {
            record["placeholder"] = json!(p);
        }
        if let Some(tc) = opts.tool_call_id {
            record["toolCallId"] = json!(tc);
        }
        if let Some(ptc) = opts.parent_tool_call_id {
            record["parentToolCallId"] = json!(ptc);
        }
        route.send(&id, record);

        if sealed {
            let state_weak = Arc::downgrade(&self.state);
            let id_sealed = id.clone();
            let title_owned = title.to_owned();
            let placeholder_owned = placeholder.map(str::to_owned);
            let dh = {
                self.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .dh
                    .clone()
            };
            tokio::spawn(async move {
                match generate_secret_request_key(dh.as_ref()).await {
                    Ok(key) => {
                        let Some(state) = state_weak.upgrade() else {
                            return;
                        };
                        let route = {
                            let mut guard = state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let Some(pending) = guard.pending.get_mut(&id_sealed) else {
                                return;
                            };
                            pending.key = Some(key.private_key);
                            pending.hpke_pk = Some(key.hpke_pk.clone());
                            guard.route.clone()
                        };
                        route.offer_sealed(
                            &id_sealed,
                            SealedOfferDetails {
                                hpke_pk: key.hpke_pk,
                                title: title_owned,
                                placeholder: placeholder_owned,
                                deadline_at_ms: deadline_ms,
                            },
                        );
                    }
                    Err(_) => {
                        if let Some(state) = state_weak.upgrade() {
                            state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .finish(&id_sealed, SecretInputOutcome::Unavailable, None, None);
                        }
                    }
                }
            });
        }

        Ok(rx.await.ok().flatten())
    }

    pub fn has(&self, id: &str) -> bool {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.pending.contains_key(id) || guard.receipts.contains_key(id)
    }

    pub fn answered(&self) -> Vec<(String, String)> {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut items: Vec<(String, String)> = guard
            .receipt_order
            .iter()
            .filter_map(|id| {
                let receipt = guard.receipts.get(id)?.as_ref()?;
                if receipt.principal.is_none() && guard.route.accepts(id) {
                    Some((id.clone(), receipt.answer_id.clone()))
                } else {
                    None
                }
            })
            .collect();
        if items.len() > 64 {
            items.drain(0..items.len() - 64);
        }
        items
    }

    pub fn respond(&self, frame: &Value) -> Value {
        let id = frame.get("id").and_then(Value::as_str).unwrap_or("");
        let refused = |err: &str| {
            json!({
                "type": "response",
                "command": "extension_ui_response",
                "id": id,
                "success": false,
                "error": err,
                "disposition": err,
            })
        };
        let Some(request_id) = answered_ui_request_id(frame).map(str::to_owned) else {
            return refused("secret_answer_refused");
        };
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !guard.route.accepts(&request_id) {
            return refused("secret_answer_refused");
        }
        if !guard.pending.contains_key(&request_id) && !guard.receipts.contains_key(&request_id) {
            return refused("unknown_extension_ui_request");
        }
        let allowed: HashSet<&str> = [
            "type",
            "id",
            "uiRequestId",
            "answerId",
            "sessionId",
            "secret",
            "value",
            "cancelled",
        ]
        .into_iter()
        .collect();
        let Some(obj) = frame.as_object() else {
            return refused("invalid_secret_response");
        };
        let Some(answer_id) = obj.get("answerId").and_then(Value::as_str) else {
            return refused("invalid_secret_response");
        };
        let valid_payload = if let Some(val) = obj.get("value") {
            !obj.contains_key("cancelled") && val.as_str().is_some_and(is_secret_input_value)
        } else {
            obj.get("cancelled").and_then(Value::as_bool) == Some(true)
        };
        if obj.get("secret").and_then(Value::as_bool) != Some(true)
            || !obj.get("id").is_some_and(Value::is_string)
            || answer_id.is_empty()
            || answer_id.encode_utf16().count() > 256
            || obj.keys().any(|k| !allowed.contains(k.as_str()))
            || !valid_payload
        {
            return refused("invalid_secret_response");
        }
        let Some(pending_deadline) = guard.pending.get(&request_id).map(|p| p.deadline_ms) else {
            return match guard.receipts.get(&request_id) {
                Some(Some(receipt))
                    if receipt.answer_id == answer_id && receipt.principal.is_none() =>
                {
                    json!({
                        "type": "response",
                        "command": "extension_ui_response",
                        "id": id,
                        "success": true,
                        "disposition": "replay",
                    })
                }
                Some(Some(_)) => refused("already_answered"),
                _ => refused("unknown_extension_ui_request"),
            };
        };
        if now_ms() >= pending_deadline {
            guard.finish(&request_id, SecretInputOutcome::TimedOut, None, None);
            return refused("unknown_extension_ui_request");
        }
        (guard.admit)();
        let cancelled = obj.get("cancelled").and_then(Value::as_bool) == Some(true);
        let value = obj.get("value").and_then(Value::as_str).map(str::to_owned);
        let answer_id_owned = answer_id.to_owned();
        guard.finish(
            &request_id,
            if cancelled {
                SecretInputOutcome::Cancelled
            } else {
                SecretInputOutcome::Answered
            },
            value,
            None,
        );
        guard.receipts.insert(
            request_id,
            Some(Receipt {
                answer_id: answer_id_owned,
                principal: None,
            }),
        );
        json!({
            "type": "response",
            "command": "extension_ui_response",
            "id": id,
            "success": true,
            "disposition": "applied",
        })
    }

    pub fn answer_local(&self, id: &str, value: Option<String>) {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !guard.pending.contains_key(id) {
            return;
        }
        let outcome = if value.is_none() {
            SecretInputOutcome::Cancelled
        } else {
            SecretInputOutcome::Answered
        };
        guard.finish(id, outcome, value, None);
        guard.receipts.insert(
            id.to_owned(),
            Some(Receipt {
                answer_id: "terminal".to_owned(),
                principal: None,
            }),
        );
    }

    pub fn cancel_all(&self, outcome: SecretInputOutcome) {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ids: Vec<String> = guard.pending.keys().cloned().collect();
        for id in ids {
            guard.finish(&id, outcome, None, None);
        }
    }

    pub fn recheck(&self) {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let unavailable: Vec<String> = guard
            .pending
            .keys()
            .filter(|id| !guard.route.available(id))
            .cloned()
            .collect();
        for id in unavailable {
            guard.finish(&id, SecretInputOutcome::Unavailable, None, None);
        }
    }

    pub fn dispose(&self) {
        self.close();
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .route
            .bind_sealed_responder(None);
    }

    pub fn close(&self) {
        self.cancel_all(SecretInputOutcome::Cancelled);
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ids: Vec<String> = guard.receipts.keys().cloned().collect();
        for id in ids {
            guard.route.forget(&id);
        }
        guard.receipts.clear();
        guard.receipt_order.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestRoute {
        latch: Mutex<Option<Arc<dyn SealedLatch>>>,
        sent: Mutex<Vec<Value>>,
    }

    impl SecretInputRoute for TestRoute {
        fn select(
            &self,
            _: &str,
            _: Box<dyn FnOnce() + Send + Sync>,
        ) -> Option<SecretInputUnavailableReason> {
            None
        }
        fn accepts(&self, _: &str) -> bool {
            true
        }
        fn available(&self, _: &str) -> bool {
            true
        }
        fn presenter(&self, _: &str) -> Option<String> {
            Some("c1".into())
        }
        fn send(&self, _: &str, record: Value) {
            self.sent
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(record);
        }
        fn settled(&self, _: &str) {}
        fn forget(&self, _: &str) {}
        fn bind_sealed_responder(&self, latch: Option<Arc<dyn SealedLatch>>) {
            *self
                .latch
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = latch;
        }
    }

    #[tokio::test]
    async fn raiser_respond_handles_applied_replay_and_already_answered() {
        let route = Arc::new(TestRoute {
            latch: Mutex::new(None),
            sent: Mutex::new(Vec::new()),
        });
        let admitted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let a_clone = admitted.clone();
        let reqs = SecretInputRequests::new(
            route.clone(),
            Arc::new(move || {
                a_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
        );
        let (id_tx, id_rx) = tokio::sync::oneshot::channel();
        let id_tx = Mutex::new(Some(id_tx));
        let reqs_clone = reqs.clone();
        let input_task = tokio::spawn(async move {
            reqs_clone
                .input(
                    "Token",
                    None,
                    None,
                    Some(&|id| {
                        if let Some(tx) = id_tx
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take()
                        {
                            let _ = tx.send(id.to_owned());
                        }
                    }),
                )
                .await
                .expect("input")
        });
        let req_id = id_rx.await.expect("req_id");
        let r1 = reqs.respond(&json!({
            "type": "extension_ui_response",
            "id": "cmd-1",
            "uiRequestId": req_id,
            "answerId": "ans-1",
            "secret": true,
            "value": "sk-123",
        }));
        assert_eq!(r1["disposition"], "applied");
        assert_eq!(input_task.await.expect("task").as_deref(), Some("sk-123"));
        assert_eq!(admitted.load(std::sync::atomic::Ordering::SeqCst), 1);

        let replay = reqs.respond(&json!({
            "type": "extension_ui_response",
            "id": "cmd-2",
            "uiRequestId": req_id,
            "answerId": "ans-1",
            "secret": true,
            "value": "sk-123",
        }));
        assert_eq!(replay["disposition"], "replay");

        let conflict = reqs.respond(&json!({
            "type": "extension_ui_response",
            "id": "cmd-3",
            "uiRequestId": req_id,
            "answerId": "ans-2",
            "secret": true,
            "value": "sk-other",
        }));
        assert_eq!(conflict["disposition"], "already_answered");
        reqs.dispose();
    }
}
