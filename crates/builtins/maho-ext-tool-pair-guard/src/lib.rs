//! Keeps provider tool-call and result pairs balanced before dispatch.
pub mod sanitize_openai_chat_completions_payload;
pub mod sanitize_openai_responses_payload;

use maho_ext_api::{EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent};
use serde_json::Value;
use std::sync::Arc;

pub struct ToolPairGuard;

pub fn sanitize_payload(payload: &Value) -> Value {
    sanitize_payload_if_changed(payload).unwrap_or_else(|| payload.clone())
}

/// Chains the sanitizers on borrows; `None` when the result equals the input payload.
pub fn sanitize_payload_if_changed(payload: &Value) -> Option<Value> {
    let anthropic = payload
        .as_object()
        .and_then(maho_ai::api::anthropic_tool_pairs::sanitize_anthropic_tool_pairs_if_changed)
        .map(Value::Object);
    let base = anthropic.as_ref().unwrap_or(payload);
    let responses = sanitize_openai_responses_payload::sanitize_openai_responses_payload_if_changed(base);
    let chat = sanitize_openai_chat_completions_payload::sanitize_openai_chat_completions_payload_if_changed(responses.as_ref().unwrap_or(base));
    chat.or(responses).or(anthropic).filter(|sanitized| sanitized != payload)
}
impl Extension for ToolPairGuard {
    fn register(&self, api: &mut ExtensionApi) {
        api.on(EventKind::BeforeProviderRequest, Arc::new(|event, _ctx| Box::pin(async move {
            let ExtensionEvent::BeforeProviderRequest { payload, .. } = event else {
                return Ok(EventResult::None);
            };
            Ok(sanitize_payload_if_changed(payload).map_or(EventResult::None, EventResult::ProviderPayload))
        })));
    }
}
