use maho_ai::types::Message;
use serde_json::Value;
use crate::{context_reduction::{ReduceContextOptions, reduce_context_messages, should_apply_context_reduction}, emergency_prune::{EmergencyPruneLatch, hard_limit_emergency_prune}, orchestration::{admit_context_tool_results, inject_token_budget_reminder}, overflow_retry::estimate_total_tokens, repair_tool_pairs::repair_orphaned_tool_results};

pub struct CompactionContextInput<'a> {
    pub messages: &'a [Value],
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
}

fn typed_messages(messages: &[Value]) -> Result<Vec<Message>, serde_json::Error> {
    maho_core::messages::convert_to_llm(messages).into_iter().map(serde_json::from_value).collect()
}

pub fn build_compaction_context(input: CompactionContextInput<'_>) -> Result<CompactionContextOutput, serde_json::Error> {
    if input.lane_owns_compaction {
        return Ok(CompactionContextOutput { messages: repair_orphaned_tool_results(&typed_messages(input.messages)?, input.now), emergency_prune_tokens: None });
    }
    let admitted = admit_context_tool_results(input.messages, input.context_window, input.tool_admission_enabled);
    let source = if input.breaker_fallback || should_apply_context_reduction(input.usage_tokens, input.context_window as f64, None, input.provider_native_path) {
        reduce_context_messages(&typed_messages(&admitted)?, &ReduceContextOptions::builtin()).messages.into_iter().map(serde_json::to_value).collect::<Result<Vec<_>, _>>()?
    } else { admitted };
    let emergency = hard_limit_emergency_prune(&source, input.prompt_context_window, Some(input.emergency_prune_latch));
    let emergency_prune_tokens = (emergency.needs_aggressive_compaction || emergency.messages != source)
        .then(|| (estimate_total_tokens(&source), estimate_total_tokens(&emergency.messages)));
    let reminded = inject_token_budget_reminder(&emergency.messages, input.reminder);
    Ok(CompactionContextOutput { messages: repair_orphaned_tool_results(&typed_messages(&reminded)?, input.now), emergency_prune_tokens })
}
