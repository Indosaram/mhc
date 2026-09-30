//! Port of senpi packages/ai/src/utils/server-fallback-receipt.ts.

use crate::model::Model;
use crate::types::{AllowedFallbackModel, AssistantMessage, ContentBlock, AssistantStopDetails, ProviderNativeContent, StopReason};
use crate::utils::diagnostics::{AssistantMessageDiagnostic, append_assistant_message_diagnostic, now_ms};
use serde_json::{Map, Value, json};

pub const SERVER_FALLBACK_ABORTED_DIAGNOSTIC: &str = "server_fallback_aborted";
pub const BILLING_INCOMPLETE_DIAGNOSTIC: &str = "billing_incomplete_after_client_abort";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerFallbackReceipt {
    pub from: String,
    pub to: String,
}

fn read_model(value: Option<&Value>) -> Option<String> {
    match value?.as_object()?.get("model")? {
        Value::String(model) if !model.is_empty() => Some(model.clone()),
        _ => None,
    }
}

pub fn parse_server_fallback_receipt(block: &Value) -> Option<ServerFallbackReceipt> {
    let block = block.as_object()?;
    if block.get("type").and_then(Value::as_str) != Some("fallback") {
        return None;
    }
    Some(ServerFallbackReceipt { from: read_model(block.get("from"))?, to: read_model(block.get("to"))? })
}

pub fn parse_sticky_fallback_receipt(usage: &Value, requested_model: &str, served_model: Option<&str>) -> Option<ServerFallbackReceipt> {
    let iterations = usage.as_object()?.get("iterations")?.as_array()?;
    let entry = iterations
        .iter()
        .rev()
        .filter_map(Value::as_object)
        .find(|entry| entry.get("type").and_then(Value::as_str) == Some("fallback_message"))?;
    let entry_model = entry.get("model").and_then(Value::as_str).filter(|m| !m.is_empty());
    let to = entry_model.or(served_model)?;
    Some(ServerFallbackReceipt { from: requested_model.to_owned(), to: to.to_owned() })
}

pub fn server_fallback_refusal_explanation(receipt: &ServerFallbackReceipt) -> String {
    format!("Server-side fallback ({} -> {}) aborted by client policy", receipt.from, receipt.to)
}

/// Accepts a server fallback: prior tool calls become discarded provider-native blocks and the
/// returned model bills at the fallback's cost when the compat allow list names it.
pub fn apply_server_fallback_continuation(message: &mut AssistantMessage, model: &Model, receipt: &ServerFallbackReceipt) -> Model {
    for block in &mut message.content {
        if matches!(block, ContentBlock::ToolCall(_)) {
            // TS keeps the whole block (with its `type` tag) as `raw`.
            let raw = serde_json::to_value(&*block).unwrap_or(Value::Null);
            *block = ContentBlock::ProviderNative(ProviderNativeContent { subtype: "discarded_tool_call".into(), raw });
        }
    }
    message.model = receipt.to.clone();
    let fallback_cost = model.compat.as_ref().and_then(|compat| {
        compat.anthropic_messages().allowed_fallback_models?.into_iter().find_map(|candidate| match candidate {
            AllowedFallbackModel::Model(fallback) if fallback.provider == model.provider && fallback.model == receipt.to => Some(fallback.cost),
            _ => None,
        })
    });
    Model { id: receipt.to.clone(), cost: fallback_cost.unwrap_or_else(|| model.cost.clone()), ..model.clone() }
}

fn details(value: Value) -> Option<Map<String, Value>> {
    value.as_object().cloned()
}

pub fn apply_server_fallback_abort(message: &mut AssistantMessage, receipt: &ServerFallbackReceipt) {
    let explanation = server_fallback_refusal_explanation(receipt);
    message.content.clear();
    message.stop_reason = StopReason::Error;
    message.stop_details = Some(AssistantStopDetails::Refusal { explanation: Some(explanation.clone()) });
    message.error_message = Some(explanation);
    append_assistant_message_diagnostic(
        &mut message.diagnostics,
        AssistantMessageDiagnostic {
            kind: SERVER_FALLBACK_ABORTED_DIAGNOSTIC.into(),
            timestamp: now_ms(),
            error: None,
            details: details(json!({ "from": receipt.from, "to": receipt.to })),
        },
    );
    append_assistant_message_diagnostic(
        &mut message.diagnostics,
        AssistantMessageDiagnostic {
            kind: BILLING_INCOMPLETE_DIAGNOSTIC.into(),
            timestamp: now_ms(),
            error: None,
            details: details(json!({ "reason": "per-attempt usage does not arrive after a client abort" })),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_block_and_sticky_receipts() {
        let block = json!({"type": "fallback", "from": {"model": "a"}, "to": {"model": "b"}});
        assert_eq!(parse_server_fallback_receipt(&block), Some(ServerFallbackReceipt { from: "a".into(), to: "b".into() }));
        assert_eq!(parse_server_fallback_receipt(&json!({"type": "fallback", "from": {"model": ""}, "to": {"model": "b"}})), None);
        let usage = json!({"iterations": [{"type": "fallback_message", "model": "x"}, {"type": "message"}, {"type": "fallback_message"}]});
        assert_eq!(parse_sticky_fallback_receipt(&usage, "req", Some("served")).map(|r| r.to), Some("served".into()));
        assert_eq!(parse_sticky_fallback_receipt(&usage, "req", None), None);
        assert_eq!(parse_sticky_fallback_receipt(&json!({"iterations": []}), "req", None), None);
    }

    #[test]
    fn abort_turns_the_message_into_a_refusal_with_diagnostics() {
        let model = crate::models_generated::get_builtin_model("anthropic", "claude-opus-4-8").expect("model");
        let mut message = crate::utils::lazy::setup_error_message(model, "x");
        let receipt = ServerFallbackReceipt { from: "a".into(), to: "b".into() };
        apply_server_fallback_abort(&mut message, &receipt);
        assert_eq!(message.error_message.as_deref(), Some("Server-side fallback (a -> b) aborted by client policy"));
        let kinds: Vec<_> = message.diagnostics.iter().flatten().map(|d| d.kind.as_str()).collect();
        assert_eq!(kinds, [SERVER_FALLBACK_ABORTED_DIAGNOSTIC, BILLING_INCOMPLETE_DIAGNOSTIC]);
        let next = apply_server_fallback_continuation(&mut message, model, &receipt);
        assert_eq!((next.id.as_str(), message.model.as_str(), &next.cost), ("b", "b", &model.cost));
    }
}
