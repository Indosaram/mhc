//! Sender half of a delivery.
//!
//! Upstream counterpart: `thread/gateway/engine.ts`: resolve, one `BEGIN IMMEDIATE` transaction that
//! writes the row, its receipt, causal edge, budgets and the target's inbox marker, then a
//! best-effort `wake` of a routable endpoint. The result is read back from the target's row, which
//! only the target's own drain moves out of `queued`.

use std::sync::Arc;

use serde_json::Value;

use super::adapter::{BoxFuture, EndpointLiveness, GatewayEndpointPort, GatewayEndpointRef};
use super::constants::TARGET_MAX_BYTES;
use super::decision::{DeliveryDecision, RefusalKind, TargetState, decide_delivery};
use super::errors::ThreadToolFailure;
use super::provenance_resolver::{ProvenanceRuntime, resolve_provenance};
use super::result::{engine_failure, result_from_row};
use super::store_api::GatewayStoreApi;
use super::store_relay_ops::hash_args;
use super::types::{
    DeliveryEnvelope, EnqueueOutcome, EnqueueRequest, EnvelopeOrigin, ExternalAuthor, GatewayDeliveryMode, GatewayDeliveryResult, GatewaySender, Provenance, ReceiptRef,
};

/// The endpoint serving a thread right now, or null when nothing answers for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayTarget {
    pub durable_id: String,
    pub endpoint: Option<GatewayEndpointRef>,
    pub liveness: EndpointLiveness,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GatewayResolution {
    Ok(GatewayTarget),
    Error(ThreadToolFailure),
}

/// Resolves an address through the caller's live-and-disk address book.
pub type GatewayResolve = Arc<dyn Fn(&str, bool) -> BoxFuture<GatewayResolution> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayDeliverRequest {
    pub sender: GatewaySender,
    pub target: String,
    pub text: String,
    pub mode: Option<GatewayDeliveryMode>,
    pub expected_turn_id: Option<i64>,
    pub all_scope: bool,
    pub idempotency_key: Option<String>,
    pub root_id: Option<String>,
}

impl GatewayDeliverRequest {
    pub fn new(sender: GatewaySender, target: impl Into<String>, text: impl Into<String>) -> Self {
        Self { sender, target: target.into(), text: text.into(), mode: None, expected_turn_id: None, all_scope: false, idempotency_key: None, root_id: None }
    }
}

pub struct GatewayEngineOptions {
    /// Sender-owned context, kept outside every delivery/tool request.
    pub sender_provenance: Option<Arc<dyn Fn() -> Provenance + Send + Sync>>,
    pub store: Arc<dyn GatewayStoreApi>,
    pub endpoints: GatewayEndpointPort,
    pub resolve: GatewayResolve,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
}

#[derive(Clone)]
pub struct GatewayEngine {
    options: Arc<GatewayEngineOptions>,
}

struct SenderFacts {
    provenance: Provenance,
    principal: String,
    rate_principal: String,
    node: String,
    turn: Option<String>,
    cause: Option<String>,
    actor: String,
    origin: EnvelopeOrigin,
    binding: Option<super::types::BindingRef>,
}

fn sender_facts(sender: &GatewaySender, resolved: Provenance) -> SenderFacts {
    let session_label = match sender {
        GatewaySender::Session { name, .. } => name.as_ref().map(|name| name.trim().to_owned()).filter(|name| !name.is_empty()),
        _ => None,
    };
    let provenance = Provenance { session: session_label.or(resolved.session.clone()), ..resolved };
    match sender {
        GatewaySender::Session { durable_id, name, turn_id, cause_delivery_id } => SenderFacts {
            provenance,
            principal: format!("session:{durable_id}"),
            rate_principal: format!("session:{durable_id}"),
            node: durable_id.clone(),
            turn: turn_id.clone(),
            cause: cause_delivery_id.clone(),
            actor: name.clone().unwrap_or_else(|| durable_id.clone()),
            origin: EnvelopeOrigin::Session { session: durable_id.clone() },
            binding: None,
        },
        GatewaySender::Cli { uid, user } => {
            let principal = format!("cli:{uid}");
            SenderFacts {
                provenance,
                principal: principal.clone(),
                rate_principal: principal.clone(),
                node: principal,
                turn: None,
                cause: None,
                actor: user.clone(),
                origin: EnvelopeOrigin::Session { session: user.clone() },
                binding: None,
            }
        }
        GatewaySender::External { origin, binding_id, binding_revision } => {
            let principal = format!("binding:{binding_id}");
            // Each human in a bound thread gets their own pair bucket; a sender with no author shares the binding's.
            let rate_principal = match &origin.author {
                Some(author) => format!("{principal}#author:{}", author.platform_user_id),
                None => principal.clone(),
            };
            SenderFacts {
                provenance,
                principal,
                rate_principal,
                node: format!("binding:{binding_id}"),
                turn: None,
                cause: None,
                actor: origin.account_id.clone(),
                origin: EnvelopeOrigin::External { external: origin.clone() },
                binding: Some(super::types::BindingRef { binding_id: binding_id.clone(), revision: *binding_revision }),
            }
        }
    }
}

/// The arguments a delivery receipt is keyed by: a retry with the same ones replays, different ones conflict.
pub fn delivery_args_hash(target: &str, text: &str, mode: GatewayDeliveryMode, expected_turn_id: Option<i64>, binding: Option<&super::types::BindingRef>, author: Option<&ExternalAuthor>) -> String {
    let mut fields = serde_json::Map::new();
    fields.insert("target".into(), Value::String(target.to_owned()));
    fields.insert("text".into(), Value::String(text.to_owned()));
    fields.insert("mode".into(), Value::String(mode.as_str().to_owned()));
    fields.insert("expected_turn_id".into(), expected_turn_id.map_or(Value::Null, Value::from));
    fields.insert("binding".into(), binding.map_or(Value::Null, |binding| serde_json::to_value(binding).unwrap_or(Value::Null)));
    fields.insert("author".into(), author.map_or(Value::Null, |author| serde_json::to_value(author).unwrap_or(Value::Null)));
    hash_args(&Value::Object(fields))
}

fn fail(code: &str, message: impl Into<String>, details: Option<Value>) -> GatewayDeliveryResult {
    GatewayDeliveryResult::Error { error: engine_failure(code, message, details) }
}

impl GatewayEngine {
    pub fn new(options: GatewayEngineOptions) -> Self {
        Self { options: Arc::new(options) }
    }

    /// The seams this engine was built with, for a caller that builds a second engine on the same ones.
    pub fn options(&self) -> &GatewayEngineOptions {
        &self.options
    }

    pub fn deliver(&self, request: GatewayDeliverRequest) -> BoxFuture<GatewayDeliveryResult> {
        let engine = self.clone();
        Box::pin(async move { engine.deliver_inner(request).await })
    }

    fn now(&self) -> i64 {
        match &self.options.now {
            Some(now) => now(),
            None => self.options.store.now(),
        }
    }

    fn sender_provenance(&self) -> Provenance {
        match &self.options.sender_provenance {
            Some(provenance) => provenance(),
            None => resolve_provenance(&ProvenanceRuntime::default()),
        }
    }

    async fn deliver_inner(&self, request: GatewayDeliverRequest) -> GatewayDeliveryResult {
        let mode = request.mode.unwrap_or(GatewayDeliveryMode::Auto);
        if mode == GatewayDeliveryMode::Steer && request.expected_turn_id.is_none() {
            return fail("invalid_arguments", "A steer needs expected_turn_id (the target's current turn_epoch).", None);
        }
        let bytes = request.text.len();
        if bytes > TARGET_MAX_BYTES {
            return fail("message_too_large", format!("The message is {bytes} bytes, above the {TARGET_MAX_BYTES}-byte limit."), None);
        }
        let resolved = (self.options.resolve)(&request.target, request.all_scope).await;
        let target = match resolved {
            GatewayResolution::Error(error) => return GatewayDeliveryResult::Error { error },
            GatewayResolution::Ok(target) => target,
        };
        let endpoint = if target.liveness == EndpointLiveness::Routable { target.endpoint.clone() } else { None };
        if endpoint.is_none() && matches!(decide_delivery(TargetState::Offline, mode, request.expected_turn_id, None), DeliveryDecision::Refuse { .. }) {
            return fail("turn_conflict", "The target has no live turn to steer.", Some(serde_json::json!({ "target": target.durable_id })));
        }
        let facts = sender_facts(&request.sender, self.sender_provenance());
        let delivery_id = super::store_ops::new_id("delivery");
        let idempotency_key = request.idempotency_key.clone().unwrap_or_else(|| format!("auto:{delivery_id}"));
        let author = match &request.sender {
            GatewaySender::External { origin, .. } => origin.author.as_ref(),
            _ => None,
        };
        let enqueue = EnqueueRequest {
            provenance: facts.provenance.clone(),
            now: self.now(),
            delivery_id: delivery_id.clone(),
            target_durable_id: target.durable_id.clone(),
            sender_principal: facts.principal.clone(),
            rate_principal: Some(facts.rate_principal.clone()),
            sender_node: facts.node.clone(),
            sender_turn: facts.turn.clone(),
            cause_delivery_id: facts.cause.clone(),
            claimed_root_id: request.root_id.clone(),
            origin: facts.origin.clone(),
            actor: facts.actor.clone(),
            body: request.text.clone(),
            mode,
            expected_turn_id: request.expected_turn_id,
            binding: facts.binding.clone(),
            receipt: ReceiptRef {
                idempotency_key: idempotency_key.clone(),
                args_hash: delivery_args_hash(&target.durable_id, &request.text, mode, request.expected_turn_id, facts.binding.as_ref(), author),
            },
            endpoint_kind: endpoint.as_ref().map(|endpoint| endpoint.kind),
        };
        let outcome = match self.options.store.enqueue(&enqueue) {
            Ok(outcome) => outcome,
            Err(error) => return fail("internal_error", error.message(), None),
        };
        match outcome {
            EnqueueOutcome::Busy => fail("overloaded", "The gateway store is locked by a suspended writer; nothing was sent.", Some(serde_json::json!({ "budget": "store_lock" }))),
            EnqueueOutcome::Refused { code, message, details } => fail(code.as_str(), message, details),
            EnqueueOutcome::Replay { result } => match serde_json::from_value::<GatewayDeliveryResult>(result) {
                Ok(GatewayDeliveryResult::Ok { thread_id, delivery_id, message_seq, delivery, effective_mode, endpoint_kind, .. }) => {
                    GatewayDeliveryResult::Ok { thread_id, delivery_id, message_seq, delivery, effective_mode, endpoint_kind, deduplicated: true }
                }
                Ok(other) => other,
                Err(error) => fail("internal_error", error.to_string(), None),
            },
            EnqueueOutcome::Inserted { row, queue_position } => {
                let settle = |result: GatewayDeliveryResult| {
                    let serialized = serde_json::to_value(&result).unwrap_or(Value::Null);
                    let _ = self.options.store.complete_receipt(self.now(), &facts.principal, &idempotency_key, &serialized);
                    result
                };
                let Some(endpoint) = endpoint else {
                    return settle(result_from_row(&row, queue_position, None, false, true));
                };
                let reply = (self.options.endpoints.wake)(endpoint.clone(), vec![delivery_id.clone()]).await;
                let reached = reply.unroutable.is_none();
                match self.options.store.delivery_view(&delivery_id) {
                    Ok(Some(view)) => settle(result_from_row(&view.row, view.queue_position, Some(endpoint.kind), false, !reached)),
                    Ok(None) => fail("internal_error", format!("delivery {delivery_id} vanished after commit"), None),
                    Err(error) => fail("internal_error", error.message(), None),
                }
            }
        }
    }
}

/// The envelope a stored row carries, for callers that need the provenance back.
pub fn envelope_of(row: &super::types::DeliveryRow) -> DeliveryEnvelope {
    DeliveryEnvelope {
        provenance: row.envelope.provenance.clone().unwrap_or(Provenance { workspace: "unclassified".into(), project: "unknown".into(), lane: None, session: None }),
        origin: row.envelope.origin.clone(),
        actor: row.envelope.actor.clone(),
        via: row.envelope.via.clone(),
        root_id: row.envelope.root_id.clone(),
        hop: row.envelope.hop,
        delivery_id: row.envelope.delivery_id.clone(),
    }
}

/// `resolveFromEntries`: the address book's entries behind the engine's resolve seam.
pub fn resolve_from_entries(entries: Arc<dyn Fn() -> Vec<GatewayAddressEntry> + Send + Sync>, caller_workspace_root: Arc<dyn Fn() -> String + Send + Sync>) -> GatewayResolve {
    Arc::new(move |address: &str, _all_scope: bool| {
        let address = address.to_owned();
        let known = entries();
        let _ = caller_workspace_root();
        Box::pin(async move {
            let found = known.into_iter().find(|entry| entry.thread_id == address || entry.name == address);
            match found {
                Some(entry) => GatewayResolution::Ok(GatewayTarget { durable_id: entry.thread_id, endpoint: entry.endpoint, liveness: entry.liveness }),
                None => GatewayResolution::Error(engine_failure("not_found", format!("No thread matches {address}."), None)),
            }
        }) as BoxFuture<GatewayResolution>
    })
}

/// One address-book entry as the engine reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayAddressEntry {
    pub thread_id: String,
    pub name: String,
    pub endpoint: Option<GatewayEndpointRef>,
    pub liveness: EndpointLiveness,
}

/// The refusal reason a decision carries, for a caller that needs it alone.
pub fn refusal_reason(decision: &DeliveryDecision) -> Option<RefusalKind> {
    match decision {
        DeliveryDecision::Refuse { reason } => Some(*reason),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    #[test]
    fn a_delivery_receipt_hash_changes_with_its_arguments() {
        let left = delivery_args_hash("receiver", "hello", GatewayDeliveryMode::Auto, None, None, None);
        let right = delivery_args_hash("receiver", "hello", GatewayDeliveryMode::Auto, None, None, None);
        let other = delivery_args_hash("receiver", "hello", GatewayDeliveryMode::FollowUp, None, None, None);
        assert_eq!(left, right);
        assert_ne!(left, other);
    }

    #[test]
    fn a_steer_without_an_expected_turn_is_refused_before_resolution() {
        let store: Arc<dyn crate::thread::store_api::GatewayStoreApi> = crate::thread::store::create_gateway_store(crate::thread::store::GatewayStoreOptions::new(std::env::temp_dir().join("maho-engine-test").to_string_lossy().into_owned())).expect("store");
        let engine = GatewayEngine::new(GatewayEngineOptions {
            sender_provenance: None,
            store,
            endpoints: GatewayEndpointPort::inert(),
            resolve: Arc::new(|_, _| Box::pin(async { GatewayResolution::Error(engine_failure("not_found", "unused", None)) })),
            now: Some(Arc::new(|| 1)),
        });
        let mut request = GatewayDeliverRequest::new(GatewaySender::Session { durable_id: "sender".into(), name: None, turn_id: None, cause_delivery_id: None }, "receiver", "hi");
        request.mode = Some(GatewayDeliveryMode::Steer);
        match block_on(engine.deliver(request)) {
            GatewayDeliveryResult::Error { error } => assert_eq!(error.code.as_str(), "invalid_arguments"),
            other => panic!("expected error, got {other:?}"),
        }
    }
}
