use maho_ai::types::Message;
use serde_json::Value;
use crate::{context_reduction::{ReduceContextOptions, reduce_context_messages}, context_reduction_frontier::{reduce_context_with_frontier, ContextReductionState}, emergency_prune::{EmergencyPruneLatch, engaged_emergency_prune}, openai_remote::{OpenAiRemoteReplayBoundary, mark_openai_remote_replay_boundary}, orchestration::{admit_owned_context_tool_results, inject_token_budget_reminder}, overflow_retry::estimate_total_tokens, repair_tool_pairs::repair_orphaned_tool_results};

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
    pub context_reduction_state: Option<&'a mut ContextReductionState>,
    pub unreduced_messages: Option<&'a [Message]>,
    pub context_overhead_tokens: u64,
    pub reduction_ceiling_tokens: Option<u64>,
    pub reduction_block_budget_ratio: Option<f64>,
    pub append_only_transcript: bool,
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
    let native = input.provider_native_path;
    let managed = !input.append_only_transcript && !native;
    let mut reduction_state = input.context_reduction_state;
    let mut local_reduction_state = ContextReductionState::default();
    let source = if managed {
        let state = reduction_state.as_deref_mut().unwrap_or(&mut local_reduction_state);
        let unreduced_converted=typed_messages(input.messages)?;
        let reduced = reduce_context_with_frontier(&typed_messages(&admitted)?, input.unreduced_messages.unwrap_or(&unreduced_converted), state, crate::context_reduction_frontier::ReductionBudget { window: input.context_window, ceiling: input.reduction_ceiling_tokens.unwrap_or(input.prompt_context_window), overhead: input.context_overhead_tokens, block_ratio: input.reduction_block_budget_ratio, force: input.breaker_fallback });
        reduced.into_iter().map(serde_json::to_value).collect::<Result<Vec<_>, _>>()?
    } else if native && input.breaker_fallback && !input.append_only_transcript {
        reduce_context_messages(&typed_messages(&admitted)?, &ReduceContextOptions::builtin()).messages.into_iter().map(serde_json::to_value).collect::<Result<Vec<_>, _>>()?
    } else { admitted };
    let source_entry_ids = realign_entry_ids(input.messages, input.entry_ids, &source);
    // A released latch leaves `source` as-is: no token report and the same entry stamps.
    let reduction_engaged = managed && reduction_state.as_deref().unwrap_or(&local_reduction_state).engaged;
    let emergency_window = if reduction_engaged { input.prompt_context_window.saturating_sub(input.context_overhead_tokens) } else { input.prompt_context_window };
    let (pruned, entry_ids, emergency_prune_tokens, aggressive, pruned_changed) = match engaged_emergency_prune(&source, emergency_window, Some(input.emergency_prune_latch)) {
        None => (source.clone(), source_entry_ids, None, false, false),
        Some(emergency) => {
            let tokens = (emergency.needs_aggressive_compaction || emergency.messages != source)
                .then(|| (estimate_total_tokens(&source), estimate_total_tokens(&emergency.messages)));
            let entry_ids = realign_entry_ids(&source, &source_entry_ids, &emergency.messages);
            (emergency.messages, entry_ids, tokens, emergency.needs_aggressive_compaction, true)
        }
    };
    let (marked, replay_boundary) = mark_openai_remote_replay_boundary(pruned, &entry_ids, input.model, input.branch_entries);
    let reminded = if input.reminder.is_some_and(|reminder| !reminder.is_empty()) { inject_token_budget_reminder(&marked, input.reminder) } else { marked };
    let outgoing = repair_orphaned_tool_results(&typed_messages(&reminded)?, input.now);
    // senpi context-pipeline.ts:90-100: a prompt that is legal only because the emergency prune
    // rewrote it must hand off to a real compaction rather than ride the prune forever.
    if managed && (reduction_engaged || pruned_changed) {
        let overhead = input.context_overhead_tokens;
        let before_prune = estimate_total_tokens(&source).saturating_add(overhead);
        let after_prune = estimate_total_tokens(&outgoing.iter().filter_map(|message| serde_json::to_value(message).ok()).collect::<Vec<_>>()).saturating_add(overhead);
        if aggressive || after_prune > input.prompt_context_window || (after_prune < before_prune && input.emergency_prune_latch.engaged) {
            reduction_state.unwrap_or(&mut local_reduction_state).compaction_required = true;
        }
    }
    Ok(CompactionContextOutput { messages: outgoing, emergency_prune_tokens, replay_boundary })
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

    #[test]
    fn emergency_prune_that_keeps_the_prompt_legal_still_requires_compaction() {
        let oversized = "x".repeat(40_000);
        let messages = [
            json!({"role":"user","content":"task","timestamp":1}),
            json!({"role":"assistant","content":[{"type":"toolCall","id":"c","name":"read","arguments":{}}],"api":"a","provider":"p","model":"m","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"toolUse","timestamp":2}),
            json!({"role":"toolResult","toolCallId":"c","toolName":"read","content":[{"type":"text","text":oversized}],"isError":false,"timestamp":3}),
        ];
        let mut latch = EmergencyPruneLatch::default();
        let mut state = ContextReductionState { engaged: true, ..Default::default() };
        let output = build_compaction_context(CompactionContextInput {
            messages: &messages, entry_ids: &[None, None, None], model: None, branch_entries: &[], context_window: 12_000,
            prompt_context_window: 12_000, usage_tokens: None, provider_native_path: false,
            tool_admission_enabled: false, breaker_fallback: false, lane_owns_compaction: false,
            append_only_transcript: false, context_reduction_state: Some(&mut state), unreduced_messages: None,
            context_overhead_tokens: 0, reduction_ceiling_tokens: None, reduction_block_budget_ratio: None,
            emergency_prune_latch: &mut latch, reminder: None, now: 0,
        }).expect("valid context");
        assert!(output.emergency_prune_tokens.is_some());
        assert!(latch.engaged);
        assert!(state.compaction_required);
    }

    #[test]
    fn preview_context_skips_breaker_reduction() {
        let messages = [json!({"role":"user","content":"keep this transcript","timestamp":1})];
        let mut latch = EmergencyPruneLatch::default();
        let output = build_compaction_context(CompactionContextInput {
            messages: &messages, entry_ids: &[None], model: None, branch_entries: &[], context_window: 100_000,
            prompt_context_window: 100_000, usage_tokens: Some(90_000.0), provider_native_path: false,
            tool_admission_enabled: false, breaker_fallback: true, lane_owns_compaction: false,
            append_only_transcript: false, context_reduction_state: None, unreduced_messages: None,
            context_overhead_tokens: 0, reduction_ceiling_tokens: None, reduction_block_budget_ratio: None,
            emergency_prune_latch: &mut latch, reminder: None, now: 0,
        }).expect("valid context");
        assert_eq!(output.messages.len(), 1);
        assert_eq!(serde_json::to_value(&output.messages[0]).expect("message serializes")["content"], "keep this transcript");
    }
}
