//! Keeps provider tool-call and result pairs balanced before dispatch.
pub mod sanitize_openai_chat_completions_payload;
pub mod sanitize_openai_responses_payload;

use maho_ext_api::{EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent};
use serde_json::Value;
use std::sync::Arc;

pub struct ToolPairGuard;

pub fn sanitize_payload(payload: &Value) -> Value {
    let anthropic = match payload.as_object() {
        Some(object) => Value::Object(maho_ai::api::anthropic_tool_pairs::sanitize_anthropic_tool_pairs(object)),
        None => payload.clone(),
    };
    let responses = sanitize_openai_responses_payload::sanitize_openai_responses_payload(&anthropic);
    sanitize_openai_chat_completions_payload::sanitize_openai_chat_completions_payload(&responses)
}
impl Extension for ToolPairGuard {
    fn register(&self, api: &mut ExtensionApi) {
        api.on(EventKind::BeforeProviderRequest, Arc::new(|event, _ctx| Box::pin(async move {
            let ExtensionEvent::BeforeProviderRequest { payload, .. } = event else {
                return Ok(EventResult::None);
            };
            let sanitized = sanitize_payload(payload);
            Ok(if sanitized == *payload { EventResult::None } else { EventResult::ProviderPayload(sanitized) })
        })));
    }
}
