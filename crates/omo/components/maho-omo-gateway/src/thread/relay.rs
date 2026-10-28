//! The relay SDK: bindings, the outbox, reply tokens and connector inbound, over one gateway store.
//!
//! Upstream counterpart: `thread/gateway/relay.ts`. Every result is data: `{ kind: "ok", ... }` or
//! `{ kind: "error", error }` with a taxonomy code and a `next_action`.

pub type LocateCallback = Arc<dyn Fn(&str) -> BoxFuture<Option<GatewayEndpointRef>> + Send + Sync>;
use std::sync::Arc;

use serde_json::Value;

use super::adapter::{BoxFuture, GatewayEndpointPort, GatewayEndpointRef, UiAnswer, UiAnswerReply};
use super::engine::{GatewayDeliverRequest, GatewayEngine};
use super::errors::{ThreadToolFailure, thread_tool_failure};
use super::store_api::GatewayStoreApi;
use super::store_relay_ops::{
    AnswerClaim, AnswerClaimRef, AnswerDelivered, BindInput, BindingsFilter, BindingRecord, CLOSED_ELSEWHERE, InboundMode, OutboxPage, PriorAnswer, RELAY_TEXT_MAX_BYTES, RelayOutcome,
    ReportOpRequest, ReportOpResult,
};
use super::types::{ExternalAuthor, GatewayDeliveryMode, GatewayDeliveryResult, GatewaySender};

pub type RelayResult<T> = Result<T, Box<ThreadToolFailure>>;

/// Endpoint refusals meaning the session no longer waits on the request.
const SESSION_NO_LONGER_WAITS: [&str; 3] = ["question_already_resolved", "unknown_extension_ui_request", "unknown_request"];

fn next_action(code: &str) -> &'static str {
    match code {
        "binding_conflict" => "Call thread_bindings to find the binding that holds this thread, then thread_rebind it with its revision, or thread_unbind it first.",
        "binding_mismatch" => "Answer through the binding that asked the question; the question stays pending.",
        "binding_inactive" => "Bind the thread again with thread_bind.",
        "stale_revision" => "Call thread_bindings for the current revision and retry with it.",
        "stale_token" => "The question was asked under an earlier binding revision or session runtime; wait for the session to ask again.",
        "already_answered" => "Nothing to do: the question has its answer.",
        "answer_in_progress" => "Retry after a moment: another answer is still being handed to the session, and the question goes back to pending if that fails.",
        "idempotency_conflict" => "Retry with a new idempotency_key.",
        "idempotency_uncertain" => "Call thread_bindings or thread_outbox to see the current state before retrying with a new key.",
        "scope_denied" => "Report through a binding attached to this session; thread_bindings lists them.",
        "unsupported" => "Report only the events the binding subscribes to, through a binding with an outbound direction.",
        "not_found" => "Call thread_bindings for the binding ids that exist.",
        "cursor_invalid" => "Read thread_outbox again and ack a cursor it returned.",
        "host_unavailable" => "Retry when the session is running; the question stays pending.",
        "invalid_arguments" => "Fix the arguments and call again.",
        _ => "Call thread_bindings and retry after checking the binding.",
    }
}

fn failure(code: &str, message: impl Into<String>, details: Option<Value>, next: Option<&str>) -> Box<ThreadToolFailure> {
    Box::new(thread_tool_failure(code, message, next.unwrap_or_else(|| next_action(code)), details))
}

fn from_store<T>(outcome: RelayOutcome<T>) -> RelayResult<T> {
    match outcome {
        RelayOutcome::Ok { value } => Ok(value),
        RelayOutcome::Refused { code, message, details } => Err(failure(code.as_str(), message, details, None)),
    }
}

fn text_too_large(text: &str) -> Option<Box<ThreadToolFailure>> {
    (text.len() > RELAY_TEXT_MAX_BYTES).then(|| failure("message_too_large", format!("The text is {} bytes, above the {RELAY_TEXT_MAX_BYTES}-byte limit.", text.len()), None, None))
}

/// The answer shape a request kind reads, from its text.
pub fn answer_shape(kind: Option<&str>, answer: &str) -> Result<Value, String> {
    match kind {
        Some("confirm") => match answer.trim().to_ascii_lowercase().as_str() {
            "yes" | "y" | "true" | "ok" => Ok(serde_json::json!({ "confirmed": true })),
            "no" | "n" | "false" => Ok(serde_json::json!({ "confirmed": false })),
            other => Err(format!("a confirm answer reads yes or no, not {other}")),
        },
        Some("select") => Ok(serde_json::json!({ "selected": answer })),
        Some("editor") | Some("input") => Ok(serde_json::json!({ "text": answer })),
        _ => Ok(serde_json::json!({ "answer": answer })),
    }
}

pub struct GatewayRelayOptions {
    pub store: Arc<dyn GatewayStoreApi>,
    pub engine: GatewayEngine,
    pub endpoints: GatewayEndpointPort,
    /// The endpoint serving a session right now, or null when nothing answers for it.
    pub locate: LocateCallback,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
}

#[derive(Clone)]
pub struct GatewayRelay {
    options: Arc<GatewayRelayOptions>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keyed {
    pub principal: String,
    pub idempotency_key: Option<String>,
}

impl Keyed {
    pub fn new(principal: impl Into<String>) -> Self {
        Self { principal: principal.into(), idempotency_key: None }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerRequest {
    pub binding_id: String,
    pub reply_token: String,
    pub answer: String,
    pub author: Option<ExternalAuthor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundRequest {
    pub binding_id: String,
    pub event_id: String,
    pub text: String,
    pub author: Option<ExternalAuthor>,
    pub mode: Option<InboundMode>,
}

impl GatewayRelay {
    pub fn new(options: GatewayRelayOptions) -> Self {
        Self { options: Arc::new(options) }
    }

    fn now(&self) -> i64 {
        match &self.options.now {
            Some(now) => now(),
            None => self.options.store.now(),
        }
    }

    pub fn bind(&self, input: &BindInput) -> RelayResult<BindingRecord> {
        from_store(self.options.store.bind(self.now(), input).map_err(|error| failure("internal_error", error.message(), None, None))?)
    }

    pub fn unbind(&self, binding_id: &str, expected_revision: i64) -> RelayResult<(BindingRecord, bool, Vec<String>)> {
        from_store(self.options.store.unbind(self.now(), binding_id, expected_revision).map_err(|error| failure("internal_error", error.message(), None, None))?)
    }

    pub fn rebind(&self, binding_id: &str, expected_revision: i64, session_durable_id: &str) -> RelayResult<(BindingRecord, Vec<String>)> {
        from_store(
            self.options
                .store
                .rebind(self.now(), binding_id, expected_revision, session_durable_id)
                .map_err(|error| failure("internal_error", error.message(), None, None))?,
        )
    }

    pub fn bindings(&self, filter: &BindingsFilter, cursor: Option<&str>, limit: Option<i64>) -> RelayResult<(Vec<BindingRecord>, Option<String>)> {
        from_store(self.options.store.list_bindings(self.now(), filter, cursor, limit).map_err(|error| failure("internal_error", error.message(), None, None))?)
    }

    pub fn report(&self, request: &ReportOpRequest) -> RelayResult<ReportOpResult> {
        if let Some(too_large) = text_too_large(&request.text) {
            return Err(too_large);
        }
        from_store(self.options.store.report(self.now(), request).map_err(|error| failure("internal_error", error.message(), None, None))?)
    }

    pub fn outbox(&self, binding_id: &str, after_cursor: Option<i64>, limit: Option<i64>) -> RelayResult<OutboxPage> {
        from_store(self.options.store.read_outbox(self.now(), binding_id, after_cursor, limit).map_err(|error| failure("internal_error", error.message(), None, None))?)
    }

    pub fn ack(&self, binding_id: &str, cursor: i64, provider_message_id: Option<&str>) -> RelayResult<(i64, bool)> {
        from_store(self.options.store.ack_outbox(self.now(), binding_id, cursor, provider_message_id).map_err(|error| failure("internal_error", error.message(), None, None))?)
    }

    /// A claimed answer whose hand-off failed goes back to pending; one that reached the session is
    /// confirmed, so a later answer reads `already_answered` instead of `answer_in_progress`.
    pub fn answer(&self, request: &AnswerRequest) -> BoxFuture<RelayResult<(AnswerClaim, Option<ExternalAuthor>)>> {
        let relay = self.clone();
        let request = request.clone();
        Box::pin(async move {
            if let Some(too_large) = text_too_large(&request.answer) {
                return Err(too_large);
            }
            let claim = match relay.options.store.claim_answer(relay.now(), &request.binding_id, &request.reply_token, &request.answer, request.author.as_ref()) {
                Ok(outcome) => match from_store(outcome) {
                    Ok(claim) => claim,
                    Err(error) => return Err(error),
                },
                Err(error) => return Err(failure("internal_error", error.message(), None, None)),
            };
            let own = AnswerClaimRef { reply_token: request.reply_token.clone(), claimed_at: claim.claimed_at };
            let respond = relay.options.endpoints.respond_ui.clone();
            let Some(respond) = respond else {
                let _ = relay.options.store.release_answer(&own);
                return Err(failure("unsupported", "The session that asked is not reachable to take the answer.", Some(serde_json::json!({ "session": claim.session_durable_id })), None));
            };
            let endpoint = match (relay.options.locate)(&claim.session_durable_id).await {
                Some(endpoint) => endpoint,
                None => {
                    let _ = relay.options.store.release_answer(&own);
                    return Err(failure("host_unavailable", "The session that asked is not reachable to take the answer.", Some(serde_json::json!({ "session": claim.session_durable_id })), None));
                }
            };
            let fields = match answer_shape(claim.ui_request_kind.as_deref(), &request.answer) {
                Ok(fields) => fields,
                Err(reason) => {
                    let _ = relay.options.store.release_answer(&own);
                    return Err(failure("invalid_arguments", reason, Some(serde_json::json!({ "session": claim.session_durable_id })), None));
                }
            };
            let reply: UiAnswerReply = respond(endpoint, UiAnswer { ui_request_id: claim.ui_request_id.clone(), fields }).await;
            match reply {
                UiAnswerReply::Delivered => {
                    let delivered = AnswerDelivered {
                        reply_token: request.reply_token.clone(),
                        claimed_at: own.claimed_at,
                        answer: request.answer.clone(),
                        answered_by: request.author.clone(),
                    };
                    let _ = relay.options.store.confirm_answer(&delivered);
                    Ok((claim, request.author))
                }
                UiAnswerReply::Refused { error } => {
                    if let Some(prior) = claim.taken_over.clone()
                        && SESSION_NO_LONGER_WAITS.contains(&error.as_str())
                    {
                        let settled = if error == "question_already_resolved" {
                            PriorAnswer { answer: prior.answer.clone(), answered_at: prior.answered_at }
                        } else {
                            PriorAnswer { answer: None, answered_at: own.claimed_at }
                        };
                        let _ = relay.options.store.mark_prior_delivered(&own, &settled);
                        return Err(failure(
                            "already_answered",
                            format!("{CLOSED_ELSEWHERE} ({error})."),
                            Some(serde_json::json!({ "session": claim.session_durable_id, "reason": error })),
                            Some("Nothing to do: the session no longer waits for this question."),
                        ));
                    }
                    let _ = relay.options.store.release_answer(&own);
                    let malformed = error == "invalid_response" || error == "question_incomplete";
                    if error == "host_draining" {
                        return Err(failure(
                            "stale_token",
                            "The session's host is draining and could not take the answer: host_draining",
                            Some(serde_json::json!({ "session": claim.session_durable_id, "reason": error })),
                            Some("The session's host is handing its sessions over; the question stays pending. Answer it again once the host is back."),
                        ));
                    }
                    Err(failure(
                        if malformed { "invalid_arguments" } else { "stale_token" },
                        format!("The session refused the answer: {error}"),
                        Some(serde_json::json!({ "session": claim.session_durable_id, "reason": error })),
                        Some(if malformed {
                            "The session could not read this answer; answer again with text it can take. The question stays pending."
                        } else {
                            "The session refused the answer: it no longer waits on this question. Read thread_outbox for a newer question and answer that one."
                        }),
                    ))
                }
            }
        })
    }

    /// A connector's inbound message; one `event_id` is admitted once.
    pub fn inbound(&self, request: &InboundRequest) -> BoxFuture<GatewayDeliveryResult> {
        let relay = self.clone();
        let request = request.clone();
        Box::pin(async move {
            if let Some(mode) = &request.mode
                && !matches!(mode.as_str(), "auto" | "follow_up")
            {
                return GatewayDeliveryResult::Error {
                    error: *failure("invalid_arguments", format!("A binding message is delivered as auto or follow_up, never {mode}."), Some(serde_json::json!({ "mode": mode })), None),
                };
            }
            let binding = match relay.options.store.binding_view(relay.now(), &request.binding_id) {
                Ok(Some(binding)) => binding,
                Ok(None) => return GatewayDeliveryResult::Error { error: *failure("not_found", "No binding has this id.", Some(serde_json::json!({ "binding_id": request.binding_id })), None) },
                Err(error) => return GatewayDeliveryResult::Error { error: *failure("internal_error", error.message(), None, None) },
            };
            // An event the session already took keeps its stored result whatever the binding did since.
            let prior = relay.options.store.delivery_receipt(relay.now(), &format!("binding:{}", binding.binding_id), &format!("event:{}", request.event_id));
            if let Ok(Some(prior)) = prior
                && prior.binding_revision.is_some()
            {
                let recovered = if prior.completed {
                    Ok(Some(super::types::EnqueueOutcome::Replay { result: prior.result.clone() }))
                } else {
                    relay.options.store.recover_delivery(relay.now(), &format!("binding:{}", binding.binding_id), &format!("event:{}", request.event_id), &prior.args_hash)
                };
                if let Ok(Some(super::types::EnqueueOutcome::Replay { result })) = recovered
                    && let Ok(replayed) = serde_json::from_value::<GatewayDeliveryResult>(result)
                {
                    return replayed;
                }
            }
            if binding.status != super::store_relay_ops::BindingStatus::Active {
                return GatewayDeliveryResult::Error {
                    error: *failure("binding_inactive", format!("The binding is {}.", binding.status.as_str()), Some(serde_json::json!({ "binding_id": binding.binding_id, "status": binding.status.as_str() })), None),
                };
            }
            if !binding.direction.inbound {
                return GatewayDeliveryResult::Error { error: *failure("unsupported", "The binding carries no inbound direction.", Some(serde_json::json!({ "binding_id": binding.binding_id })), None) };
            }
            let mode = request.mode.clone().unwrap_or_else(|| binding.inbound_mode.clone());
            if mode == "auto" && binding.inbound_mode == "follow_up" {
                return GatewayDeliveryResult::Error {
                    error: *failure(
                        "invalid_arguments",
                        "The binding delivers at most follow_up; a message cannot ask for more than its binding's inbound_mode.",
                        Some(serde_json::json!({ "binding_id": binding.binding_id, "mode": mode, "inbound_mode": binding.inbound_mode })),
                        Some("Send with mode follow_up (or omit it), or rebind the thread with inbound_mode auto."),
                    ),
                };
            }
            let origin = super::types::ExternalOrigin {
                platform: binding.platform.clone(),
                account_id: binding.account_id.clone(),
                chat_id: binding.chat_id.clone(),
                thread_id: binding.thread_id.clone(),
                message_id: request.event_id.clone(),
                author: request.author.clone(),
            };
            let mut deliver = GatewayDeliverRequest::new(
                GatewaySender::External { origin, binding_id: binding.binding_id.clone(), binding_revision: binding.revision },
                binding.session_durable_id.clone(),
                request.text.clone(),
            );
            deliver.mode = Some(GatewayDeliveryMode::parse(&mode).unwrap_or(GatewayDeliveryMode::FollowUp));
            deliver.all_scope = true;
            deliver.idempotency_key = Some(format!("event:{}", request.event_id));
            relay.options.engine.deliver(deliver).await
        })
    }

    pub fn settle(&self, session_durable_id: &str, outcome: &str, through_arm_seq: Option<i64>) -> RelayResult<Vec<(String, i64)>> {
        self.options.store.emit_completions(self.now(), session_durable_id, outcome, through_arm_seq).map_err(|error| failure("internal_error", error.message(), None, None))
    }
}

/// The refusal a closed answer route reports while it keeps the question pending.
pub fn closed_elsewhere(error: &str) -> ThreadToolFailure {
    *failure("already_answered", format!("{CLOSED_ELSEWHERE} ({error})."), Some(serde_json::json!({ "reason": error })), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_confirm_answer_reads_yes_or_no() {
        assert_eq!(answer_shape(Some("confirm"), "Yes").unwrap(), serde_json::json!({ "confirmed": true }));
        assert!(answer_shape(Some("confirm"), "maybe").is_err());
        assert_eq!(answer_shape(Some("question"), "yes").unwrap(), serde_json::json!({ "answer": "yes" }));
        assert_eq!(answer_shape(None, "hello").unwrap(), serde_json::json!({ "answer": "hello" }));
    }

    #[test]
    fn a_text_over_the_relay_cap_is_refused() {
        let long = "a".repeat(RELAY_TEXT_MAX_BYTES + 1);
        assert_eq!(text_too_large(&long).map(|error| error.code.to_string()), Some("message_too_large".to_owned()));
        assert!(text_too_large("short").is_none());
    }

    #[test]
    fn every_taxonomy_code_has_a_next_action() {
        for code in super::super::errors::THREAD_ERROR_CODES {
            assert!(!next_action(code).is_empty());
        }
    }
}
