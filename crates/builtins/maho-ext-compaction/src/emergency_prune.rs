use maho_ext_api::ToolResult;
use serde_json::Value;
use crate::{overflow_retry::{estimate_total_tokens, prune_old_messages_to_budget}, tool_truncation::{pre_prune_tool_outputs_to_budget, truncate_oversized_tool_results}};

#[derive(Clone, Copy, Debug, Default)]
pub struct EmergencyPruneLatch { pub engaged: bool }
#[derive(Clone, Debug)]
pub struct EmergencyPruneResult { pub messages: Vec<Value>, pub needs_aggressive_compaction: bool }
fn transform_results(messages: &[Value], transform: impl FnOnce(&[ToolResult]) -> Vec<ToolResult>) -> Vec<Value> {
    let results: Vec<_> = messages.iter().filter(|m| m.get("role").and_then(Value::as_str) == Some("toolResult"))
        .map(|m| serde_json::from_value::<ToolResult>(serde_json::json!({"content":m["content"]})).expect("tool-result content follows ToolContent contract")).collect();
    if results.is_empty() { return messages.to_vec(); }
    let transformed = transform(&results);
    let mut next = transformed.into_iter();
    messages.iter().map(|message| {
        let mut message = message.clone();
        if message.get("role").and_then(Value::as_str) == Some("toolResult") && let Some(result) = next.next() {
            message["content"] = serde_json::to_value(result.content).expect("ToolContent serializes");
        }
        message
    }).collect()
}
pub fn prune_tool_results(messages: &[Value], window: u64, budget_ratio: f64) -> Vec<Value> {
    transform_results(messages, |results| pre_prune_tool_outputs_to_budget(results, (window as f64 * budget_ratio).floor() as usize))
}
pub fn truncate_context_messages(messages: &[Value]) -> Vec<Value> { transform_results(messages, truncate_oversized_tool_results) }
pub fn hard_limit_emergency_prune(messages: &[Value], window: u64, latch: Option<&mut EmergencyPruneLatch>) -> EmergencyPruneResult {
    let target = (window as f64 * 0.95).floor() as u64;
    let release = (window as f64 * 0.85).floor() as u64;
    let total = estimate_total_tokens(messages);
    let engaged = total > if latch.as_ref().is_some_and(|l| l.engaged) { release } else { target };
    if let Some(latch) = latch { latch.engaged = engaged; }
    if !engaged { return EmergencyPruneResult { messages: messages.to_vec(), needs_aggressive_compaction: false }; }
    let pruned = truncate_context_messages(&prune_tool_results(messages, window, 0.6));
    if estimate_total_tokens(&pruned) <= target { return EmergencyPruneResult { messages: pruned, needs_aggressive_compaction: false }; }
    EmergencyPruneResult { messages: prune_old_messages_to_budget(&pruned, target), needs_aggressive_compaction: true }
}
