use std::collections::HashSet;
use serde_json::Value;
use maho_core::compaction::compaction::estimate_tokens;
pub const MAX_SUMMARIZATION_OVERFLOW_RETRIES:u32=3;
pub const SUMMARIZATION_OVERFLOW_TOTAL_BUDGET_MS:f64=240000.0;
pub const SUMMARIZATION_INPUT_BUDGET_RATIO:f64=0.6;
#[derive(Debug)]
pub struct SummarizationOverflowExhaustedError {pub attempts:u32,pub elapsed_ms:f64}
impl std::fmt::Display for SummarizationOverflowExhaustedError {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {
        write!(f,"Compaction summary request exceeded the context window after {} overflow {} over {}s",self.attempts,if self.attempts==1 {"retry"} else {"retries"},(self.elapsed_ms/1000.0).round())
    }
}
impl std::error::Error for SummarizationOverflowExhaustedError {}
fn estimate_wire_tokens(message:&Value)->u64 {
    let base=estimate_tokens(message);
    let serialized=message.to_string();
    let extra:usize=serialized.chars().filter(|ch|matches!(u32::from(*ch),0x1100..=0x11ff|0x2e80..=0x9fff|0xac00..=0xd7af|0xf900..=0xfaff|0xff00..=0xffef|0x20000..=0x2fa1f)).map(|ch|ch.len_utf16()*2).sum();
    base.saturating_add(u64::try_from(extra.div_ceil(4)).unwrap_or(u64::MAX))
}
pub fn estimate_total_tokens(messages:&[Value])->u64 {messages.iter().map(estimate_wire_tokens).sum()}
fn role(message:&Value)->Option<&str> {message.get("role").and_then(Value::as_str)}
fn tool_call_ids(message:&Value)->HashSet<&str> {
    if role(message)!=Some("assistant") {return HashSet::new();}
    message.get("content").and_then(Value::as_array).into_iter().flatten()
        .filter(|block|block.get("type").and_then(Value::as_str)==Some("toolCall"))
        .filter_map(|block|block.get("id").and_then(Value::as_str)).collect()
}
pub fn prune_old_messages_to_budget(messages:&[Value],target_tokens:u64)->Vec<Value> {
    let mut pruned=messages.to_vec();
    let mut total=estimate_total_tokens(&pruned);
    while total>target_tokens {
        let boundary=pruned.iter().rposition(|message|matches!(role(message),Some("user"|"bashExecution"))).unwrap_or(pruned.len());
        let candidate=pruned.iter().take(boundary).position(|message|role(message)==Some("toolResult") || !tool_call_ids(message).is_empty())
            .or_else(||pruned.iter().take(boundary).position(|message|role(message)!=Some("toolResult")));
        let Some(index)=candidate else {break;};
        let ids:HashSet<String>=tool_call_ids(&pruned[index]).into_iter().map(str::to_owned).collect();
        let mut removed=0u64;
        pruned=pruned.into_iter().enumerate().filter_map(|(candidate,message)| {
            let remove=candidate==index || (role(&message)==Some("toolResult") && message.get("toolCallId").and_then(Value::as_str).is_some_and(|id|ids.contains(id)));
            if remove {removed=removed.saturating_add(estimate_wire_tokens(&message));None} else {Some(message)}
        }).collect();
        total=total.saturating_sub(removed);
    }
    pruned
}
pub fn summarization_history_budget(context_window:u64,prompt_tokens:u64)->u64 {
    context_window.saturating_mul(3).div_euclid(5).saturating_sub(prompt_tokens).max(256)
}
pub fn bound_summarization_input(messages:&[Value],context_window:u64,prompt_tokens:u64)->Vec<Value> {
    prune_old_messages_to_budget(messages,summarization_history_budget(context_window,prompt_tokens))
}
pub fn shrink_summarization_input_for_overflow_retry(messages:&[Value],context_window:u64,prompt_tokens:u64)->Option<Vec<Value>> {
    if messages.len()<=1 {return None;}
    let shrunk=prune_old_messages_to_budget(messages,(estimate_total_tokens(messages)/2).min(summarization_history_budget(context_window,prompt_tokens)));
    if shrunk.len()<messages.len() {Some(shrunk)} else {Some(messages[1..].to_vec())}
}
pub fn allow_overflow_retry(attempts:u32,elapsed_ms:f64)->bool {attempts<MAX_SUMMARIZATION_OVERFLOW_RETRIES && elapsed_ms<SUMMARIZATION_OVERFLOW_TOTAL_BUDGET_MS}
