use std::collections::HashSet;
use serde_json::Value;
use super::compaction::estimate_tokens;
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
fn last_user_like(messages: &[Value]) -> usize {
    messages.iter().rposition(|message| matches!(role(message), Some("user" | "bashExecution"))).unwrap_or(messages.len())
}
fn prune_summarization_input_to_budget(messages: &[Value], target: u64) -> Vec<Value> {
    let mut pruned = prune_old_messages_to_budget(messages, target);
    while estimate_total_tokens(&pruned) > target {
        let boundary = last_user_like(&pruned);
        let Some(index) = pruned.iter().enumerate().skip(boundary.saturating_add(1)).find_map(|(index, message)| (!tool_call_ids(message).is_empty()).then_some(index)) else { break; };
        let ids: HashSet<String> = tool_call_ids(&pruned[index]).into_iter().map(str::to_owned).collect();
        pruned = pruned.into_iter().enumerate().filter_map(|(candidate, message)| {
            if candidate == index || (role(&message) == Some("toolResult") && message.get("toolCallId").and_then(Value::as_str).is_some_and(|id| ids.contains(id))) { None } else { Some(message) }
        }).collect();
    }
    pruned
}
fn truncate_request_to_budget(messages: &[Value], target: u64) -> Option<Vec<Value>> {
    let index = last_user_like(messages);
    let request = messages.get(index)?;
    if role(request) != Some("user") || estimate_wire_tokens(request) <= target { return None; }
    let full = match request.get("content")? {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks.iter().map(|block| if block.get("type").and_then(Value::as_str) == Some("text") { block.get("text").and_then(Value::as_str).unwrap_or_default() } else { "" }).collect::<Vec<_>>().join("\n"),
        _ => return None,
    };
    let mut text = full.as_str();
    let mut already_cut = 0;
    if let Some((prefix, suffix)) = full.rsplit_once("\n\n[... ")
        && let Some(count) = suffix.strip_suffix(" more characters truncated]").and_then(|count| count.parse::<usize>().ok()) { text = prefix; already_cut = count; }
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= 200 { return None; }
    let available = target.saturating_sub(estimate_total_tokens(messages).saturating_sub(estimate_wire_tokens(request)));
    let mut plain = request.clone(); plain["content"] = Value::String(text.to_owned());
    let mut keep = (chars.len() as u64 * available / estimate_wire_tokens(&plain).max(1)).max(200).min(chars.len() as u64 - 1) as usize;
    loop {
        let mut truncated = request.clone();
        truncated["content"] = Value::String(format!("{}\n\n[... {} more characters truncated]", chars[..keep].iter().collect::<String>(), chars.len() - keep + already_cut));
        if estimate_wire_tokens(&truncated) <= available || keep <= 200 {
            let mut next = messages.to_vec(); next[index] = truncated; return Some(next);
        }
        keep = (keep * 4 / 5).max(200);
    }
}
pub fn bound_summarization_input(messages:&[Value],context_window:u64,prompt_tokens:u64)->Vec<Value> {
    let budget = summarization_history_budget(context_window, prompt_tokens);
    let pruned = prune_summarization_input_to_budget(messages, budget);
    if pruned.is_empty() { return messages.last().cloned().into_iter().collect(); }
    if estimate_total_tokens(&pruned) <= budget { return pruned; }
    truncate_request_to_budget(&pruned, budget).unwrap_or(pruned)
}
pub fn shrink_summarization_input_for_overflow_retry(messages:&[Value],context_window:u64,prompt_tokens:u64)->Option<Vec<Value>> {
    let target = (estimate_total_tokens(messages) / 2).min(summarization_history_budget(context_window, prompt_tokens));
    if messages.len() <= 1 { return truncate_request_to_budget(messages, target); }
    let shrunk = prune_summarization_input_to_budget(messages, target);
    if shrunk.len() < messages.len() { return Some(shrunk); }
    let boundary = last_user_like(messages);
    if boundary < messages.len() - 1 { return Some(messages.iter().enumerate().filter(|(index, _)| *index != boundary + 1).map(|(_, message)| message.clone()).collect()); }
    Some(messages[1..].to_vec())
}
pub fn allow_overflow_retry(attempts:u32,elapsed_ms:f64)->bool {attempts<MAX_SUMMARIZATION_OVERFLOW_RETRIES && elapsed_ms<SUMMARIZATION_OVERFLOW_TOTAL_BUDGET_MS}

#[cfg(test)]
mod tests {
use super::*;
use serde_json::json;
#[test] fn retry_attempts_and_elapsed_budget_are_bounded() {assert!(allow_overflow_retry(2,239999.0));assert!(!allow_overflow_retry(3,0.0));assert!(!allow_overflow_retry(0,240000.0));}
#[test] fn history_budget_leaves_prompt_room_and_floor() {assert_eq!(summarization_history_budget(8000,1000),3800);assert_eq!(summarization_history_budget(100,1000),256);}
#[test] fn old_tool_pairs_are_removed_together() {
    let messages=[json!({"role":"assistant","content":[{"type":"toolCall","id":"old","name":"bash","arguments":{}}]}),json!({"role":"toolResult","toolCallId":"old","content":[{"type":"text","text":"x".repeat(4000)}]}),json!({"role":"user","content":"latest"})];
    let pruned=prune_old_messages_to_budget(&messages,10);assert_eq!(pruned,vec![messages[2].clone()]);
}
#[test] fn last_user_and_current_turn_are_preserved() {let messages=[json!({"role":"user","content":"x".repeat(4000)}),json!({"role":"assistant","content":[{"type":"text","text":"y".repeat(4000)}]})];assert_eq!(prune_old_messages_to_budget(&messages,1),messages);}
#[test] fn oversized_input_is_presized() {let messages:Vec<_>=(0..40).map(|index|json!({"role":"assistant","content":[{"type":"text","text":format!("history-{index} {}","x".repeat(1200))}]})).collect();let bounded=bound_summarization_input(&messages,8000,100);assert!(estimate_total_tokens(&bounded)<=4700);assert!(bounded.len()<messages.len());}
#[test] fn retry_shrinks_geometrically() {let messages:Vec<_>=(0..40).map(|_|json!({"role":"assistant","content":[{"type":"text","text":"x".repeat(1200)}]})).collect();let bounded=bound_summarization_input(&messages,8000,100);let shrunk=shrink_summarization_input_for_overflow_retry(&bounded,8000,100).expect("shrink");assert!(shrunk.len()<=bounded.len().div_ceil(2));}
#[test] fn unprunable_current_turn_still_changes_retry() {let messages=[json!({"role":"user","content":"latest"}),json!({"role":"assistant","content":[{"type":"text","text":"answer"}]})];assert_eq!(shrink_summarization_input_for_overflow_retry(&messages,8000,0),Some(vec![messages[0].clone()]));}
#[test] fn one_message_cannot_shrink() {assert!(shrink_summarization_input_for_overflow_retry(&[json!({"role":"user","content":"only"})],8000,0).is_none());}
#[test] fn cjk_density_is_charged_in_wire_fields() {let ascii=json!({"role":"user","content":"x".repeat(100)});let cjk=json!({"role":"user","content":"\u{ac00}".repeat(100)});assert_eq!(estimate_total_tokens(&[ascii]),25);assert_eq!(estimate_total_tokens(&[cjk]),75);}

}
