use maho_ai::types::Message;
use serde_json::Value;
use crate::{context_reduction::{ReduceContextOptions, reduce_context_messages, should_apply_context_reduction}, emergency_prune::{EmergencyPruneLatch, engaged_emergency_prune}, openai_remote::{OpenAiRemoteReplayBoundary, mark_openai_remote_replay_boundary}, orchestration::{admit_owned_context_tool_results, inject_token_budget_reminder}, overflow_retry::estimate_total_tokens, repair_tool_pairs::repair_orphaned_tool_results};

pub struct CompactionContextInput<'a> {
    pub messages: &'a [Value],
    /// Session entry stamp per message (senpi `SESSION_CONTEXT_ENTRY_ID`), index-aligned with `messages`.
    pub entry_ids: &'a [Option<String>],
    pub model: Option<&'a maho_ai::types::Model>,
    pub branch_entries: &'a [Value],
    pub context_window: u64,
    pub prompt_context_window: u64,
    pub usage_tokens: Option<f64>,
    pub provider_native_path: bool,
    pub tool_admission_enabled: bool,
    pub breaker_fallback: bool,
    pub lane_owns_compaction: bool,
    pub emergency_prune_latch: &'a mut EmergencyPruneLatch,
    pub reminder: Option<&'a str>,
    pub now: i64,
}

pub struct CompactionContextOutput {
    pub messages: Vec<Message>,
    pub emergency_prune_tokens: Option<(u64, u64)>,
    pub replay_boundary: Option<OpenAiRemoteReplayBoundary>,
}

fn typed_messages(messages: &[Value]) -> Result<Vec<Message>, serde_json::Error> {
    maho_core::messages::convert_to_llm(messages).into_iter().map(serde_json::from_value).collect()
}

/// Identity of a message that the later stages cannot change: the emergency prune rewrites
/// tool-result content and drops whole messages, and reduction projects custom roles onto user
/// turns, so timestamp and tool-call id still pair a survivor with its source in order.
fn prune_key(message: &Value) -> [Option<&Value>; 2] { [message.get("timestamp"), message.get("toolCallId")] }

/// Carry each entry stamp onto the surviving message, the way senpi's stamp rides on the message object.
fn realign_entry_ids(source: &[Value], entry_ids: &[Option<String>], next: &[Value]) -> Vec<Option<String>> {
    if source.len() == next.len() { return (0..next.len()).map(|index| entry_ids.get(index).cloned().flatten()).collect(); }
    let mut cursor = 0;
    next.iter().map(|message| {
        let found = source[cursor..].iter().position(|candidate| prune_key(candidate) == prune_key(message))?;
        cursor += found + 1;
        entry_ids.get(cursor - 1).cloned().flatten()
    }).collect()
}

pub fn build_compaction_context(input: CompactionContextInput<'_>) -> Result<CompactionContextOutput, serde_json::Error> {
    if input.lane_owns_compaction {
        return Ok(CompactionContextOutput { messages: repair_orphaned_tool_results(&typed_messages(input.messages)?, input.now), emergency_prune_tokens: None, replay_boundary: None });
    }
    let admitted = admit_owned_context_tool_results(input.messages.to_vec(), input.context_window, input.tool_admission_enabled);
    let source = if input.breaker_fallback || should_apply_context_reduction(input.usage_tokens, input.context_window as f64, None, input.provider_native_path) {
        reduce_context_messages(&typed_messages(&admitted)?, &ReduceContextOptions::builtin()).messages.into_iter().map(serde_json::to_value).collect::<Result<Vec<_>, _>>()?
    } else { admitted };
    let source_entry_ids = realign_entry_ids(input.messages, input.entry_ids, &source);
    // A released latch leaves `source` as-is: no token report and the same entry stamps.
    let (pruned, entry_ids, emergency_prune_tokens) = match engaged_emergency_prune(&source, input.prompt_context_window, Some(input.emergency_prune_latch)) {
        None => (source, source_entry_ids, None),
        Some(emergency) => {
            let tokens = (emergency.needs_aggressive_compaction || emergency.messages != source)
                .then(|| (estimate_total_tokens(&source), estimate_total_tokens(&emergency.messages)));
            let entry_ids = realign_entry_ids(&source, &source_entry_ids, &emergency.messages);
            (emergency.messages, entry_ids, tokens)
        }
    };
    let (marked, replay_boundary) = mark_openai_remote_replay_boundary(pruned, &entry_ids, input.model, input.branch_entries);
    let reminded = if input.reminder.is_some_and(|reminder| !reminder.is_empty()) { inject_token_budget_reminder(&marked, input.reminder) } else { marked };
    Ok(CompactionContextOutput { messages: repair_orphaned_tool_results(&typed_messages(&reminded)?, input.now), emergency_prune_tokens, replay_boundary })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn entry_ids_follow_survivors_when_messages_are_dropped() {
        let source = [json!({"role":"user","timestamp":1}), json!({"role":"toolResult","toolCallId":"t","timestamp":2}), json!({"role":"user","timestamp":3})];
        let ids = [Some("a".to_owned()), Some("b".to_owned()), Some("c".to_owned())];
        assert_eq!(realign_entry_ids(&source, &ids, &[source[0].clone(), source[2].clone()]), vec![Some("a".to_owned()), Some("c".to_owned())]);
    }
}
