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
const TRUNCATED_REQUEST_MIN_CHARS: usize = 200;

fn last_user_like_index(messages: &[Value]) -> usize {
    messages.iter().rposition(|message|matches!(role(message),Some("user"|"bashExecution"))).unwrap_or(messages.len())
}

fn truncate_request_to_budget(messages: &[Value], target_tokens: u64) -> Option<Vec<Value>> {
    let index = last_user_like_index(messages);
    let request = messages.get(index)?;
    if role(request) != Some("user") { return None; }
    let request_tokens = estimate_wire_tokens(request);
    if request_tokens <= target_tokens { return None; }
    let full_text = match &request["content"] {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks.iter().map(|block| if block["type"] == "text" {block["text"].as_str().unwrap_or_default()} else {""}).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    };
    let (text, already_cut) = full_text.rsplit_once("\n\n[... ")
        .and_then(|(text,note)|note.strip_suffix(" more characters truncated]")?.parse::<usize>().ok().map(|count|(text,count)))
        .unwrap_or((&full_text,0));
    // JS measures/slices UTF-16 code units. Lossy conversion only affects the
    // unusual case of a cut between a surrogate pair, as JSON also replaces it.
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() <= TRUNCATED_REQUEST_MIN_CHARS {return None;}
    let available = target_tokens.saturating_sub(estimate_total_tokens(messages).saturating_sub(request_tokens));
    let mut text_request = request.clone(); text_request["content"] = Value::String(text.into());
    let text_tokens = estimate_wire_tokens(&text_request).max(1);
    let mut keep = (units.len().saturating_sub(1)).min(TRUNCATED_REQUEST_MIN_CHARS.max(((units.len() as u128 * u128::from(available))/u128::from(text_tokens)) as usize));
    loop {
        let mut truncated = request.clone();
        truncated["content"] = Value::String(format!("{}\n\n[... {} more characters truncated]",String::from_utf16_lossy(&units[..keep]),units.len()-keep+already_cut));
        if estimate_wire_tokens(&truncated) <= available || keep <= TRUNCATED_REQUEST_MIN_CHARS {
            let mut result = messages.to_vec();result[index]=truncated;return Some(result);
        }
        keep = TRUNCATED_REQUEST_MIN_CHARS.max(keep*4/5);
    }
}

fn prune_summarization_input_to_budget(messages: &[Value], target: u64) -> Vec<Value> {
    let mut pruned=prune_old_messages_to_budget(messages,target);
    while estimate_total_tokens(&pruned)>target {
        let boundary=last_user_like_index(&pruned);
        let index=pruned.iter().enumerate().skip(boundary.saturating_add(1)).find(|(_,message)|!tool_call_ids(message).is_empty()).map(|(index,_)|index);
        let Some(index)=index else {break;};
        let ids:HashSet<String>=tool_call_ids(&pruned[index]).into_iter().map(str::to_owned).collect();
        pruned=pruned.into_iter().enumerate().filter_map(|(candidate,message)| {
            if candidate==index || (role(&message)==Some("toolResult") && message["toolCallId"].as_str().is_some_and(|id|ids.contains(id))) {None} else {Some(message)}
        }).collect();
    }
    pruned
}

pub fn bound_summarization_input(messages:&[Value],context_window:u64,prompt_tokens:u64)->Vec<Value> {
    let budget=summarization_history_budget(context_window,prompt_tokens);
    let pruned=prune_summarization_input_to_budget(messages,budget);
    if pruned.is_empty() {return messages.last().cloned().into_iter().collect();}
    if estimate_total_tokens(&pruned)<=budget {return pruned;}
    truncate_request_to_budget(&pruned,budget).unwrap_or(pruned)
}
pub fn shrink_summarization_input_for_overflow_retry(messages:&[Value],context_window:u64,prompt_tokens:u64)->Option<Vec<Value>> {
    let target=(estimate_total_tokens(messages)/2).min(summarization_history_budget(context_window,prompt_tokens));
    if messages.len()<=1 {return truncate_request_to_budget(messages,target);}
    let shrunk=prune_summarization_input_to_budget(messages,target);
    if shrunk.len()<messages.len() {return Some(shrunk);}
    let boundary=last_user_like_index(messages);
    let remove=if boundary<messages.len()-1 {boundary+1} else {0};
    Some(messages.iter().enumerate().filter(|(index,_)|*index!=remove).map(|(_,message)|message.clone()).collect())
}
pub fn allow_overflow_retry(attempts:u32,elapsed_ms:f64)->bool {attempts<MAX_SUMMARIZATION_OVERFLOW_RETRIES && elapsed_ms<SUMMARIZATION_OVERFLOW_TOTAL_BUDGET_MS}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn oversized_request_fits_and_retry_folds_truncation_note() {
        let messages=vec![json!({"role":"user","content":"x".repeat(40000),"timestamp":1})];
        let bounded=bound_summarization_input(&messages,8000,100);
        assert!(estimate_total_tokens(&bounded)<=4700);
        let text=bounded[0]["content"].as_str().expect("text");
        assert!(text.starts_with(&"x".repeat(200)));
        assert!(text.ends_with(" more characters truncated]"));
        let retry=shrink_summarization_input_for_overflow_retry(&bounded,8000,100).expect("cut");
        let text=retry[0]["content"].as_str().expect("text");
        assert_eq!(text.matches("more characters truncated]").count(),1);
        assert!(estimate_total_tokens(&retry)<estimate_total_tokens(&bounded));
        assert_eq!(messages[0]["content"].as_str().expect("original").len(),40000);
    }
    #[test]
    fn minimum_prefix_survives_and_images_do_not_deepen_cut() {
        let request=json!({"role":"user","content":"a".repeat(40000)});
        let image=json!({"role":"user","content":[{"type":"text","text":"a".repeat(40000)},{"type":"image","data":"z".repeat(1000000)}]});
        let text=bound_summarization_input(&[request],8000,100);
        let image=bound_summarization_input(&[image],8000,100);
        let text_content=text[0]["content"].as_str().expect("text");
        let image_content=image[0]["content"].as_str().expect("text");
        assert!(text_content.len().abs_diff(image_content.len()) <= 2);
        let minimum=truncate_request_to_budget(&text,0).expect("cut");
        assert!(minimum[0]["content"].as_str().expect("content").starts_with(&"a".repeat(200)));
        assert!(truncate_request_to_budget(&[json!({"role":"user","content":"a".repeat(200)})],0).is_none());
    }
    #[test]
    fn last_turn_tool_steps_are_removed_as_pairs_but_request_is_kept() {
        let messages=vec![json!({"role":"user","content":"request"}), json!({"role":"assistant","content":[{"type":"toolCall","id":"a","arguments":{}}]}),json!({"role":"toolResult","toolCallId":"a","content":"x".repeat(40000)})];
        assert_eq!(bound_summarization_input(&messages,8000,0),messages[..1]);
        let custom=vec![json!({"role":"custom","content":"x".repeat(40000)})];
        assert_eq!(bound_summarization_input(&custom,8000,0),custom);
    }
}
