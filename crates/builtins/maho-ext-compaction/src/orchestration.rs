use maho_core::compaction::{compaction::estimate_tokens, settings::CompactionSettings};
use serde_json::{Value, json};
use crate::{idle::{IdleCompactionDecision, should_warm_at_idle}, policy::{self, CompactionYield}, speculation_lead::*, tool_admission::*};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompactionGeometry { pub reserve_tokens: f64, pub threshold_tokens: f64, pub lead_tokens: f64 }
pub fn resolve_compaction_geometry(context_window: f64, settings: &CompactionSettings, last_yield: Option<CompactionYield>) -> CompactionGeometry {
    let threshold_tokens = context_window * policy::compute_effective_threshold(context_window, last_yield);
    CompactionGeometry { reserve_tokens: policy::resolve_effective_reserve_tokens(context_window, settings.reserve_tokens as f64, settings.ideal.reserve_scaling_enabled), threshold_tokens, lead_tokens: resolve_speculation_lead_tokens(threshold_tokens, settings.ideal.speculative_lead_tokens.map(|lead| lead as f64)) }
}
pub fn should_defer_grace_band(tokens: f64, geometry: CompactionGeometry, context_window: f64, in_flight: bool, enabled: Option<bool>) -> bool {
    in_flight && enabled != Some(false) && is_within_grace_band(tokens, geometry.threshold_tokens, geometry.lead_tokens, context_window, geometry.reserve_tokens)
}
pub fn inject_token_budget_reminder(messages: &[Value], reminder: Option<&str>) -> Vec<Value> {
    let Some(reminder) = reminder.filter(|r| !r.is_empty()) else { return messages.to_vec(); };
    let mut output = messages.to_vec();
    if let Some(message) = output.iter_mut().rev().find(|m| m.get("role").and_then(Value::as_str) == Some("user")) {
        let content = match message.get("content") { Some(Value::String(s)) => vec![json!({"type":"text","text":s})], Some(Value::Array(a)) => a.clone(), _ => Vec::new() };
        if content.first().is_some_and(|b| b.get("type").and_then(Value::as_str) == Some("text") && b.get("text").and_then(Value::as_str) == Some(reminder)) { return output; }
        message["content"] = Value::Array(std::iter::once(json!({"type":"text","text":reminder})).chain(content).collect());
    }
    output
}
pub fn resolve_before_agent_start_message(message: Option<&Value>, reminder: Option<&str>, enabled: Option<bool>) -> Option<Value> {
    let mut message = message?.clone();
    if let Some(reminder) = reminder.filter(|r| !r.is_empty()) && enabled != Some(false) {
        message["content"] = json!(format!("{}\n\n{reminder}", message.get("content").and_then(Value::as_str).unwrap_or_default()));
    }
    Some(message)
}
pub fn resolve_reminder_system_prompt(prompt: &str, reminder: Option<&str>, enabled: Option<bool>) -> Option<String> {
    reminder.filter(|r| !r.is_empty() && enabled != Some(false)).map(|r| format!("{prompt}\n\n{r}"))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdleWarmAction { None, Start, Replace }
pub fn resolve_idle_warm_action(decision: &IdleCompactionDecision<'_>, armed_at_tokens: Option<f64>) -> IdleWarmAction {
    if !should_warm_at_idle(decision) { return IdleWarmAction::None; }
    match armed_at_tokens {
        None => IdleWarmAction::Start,
        Some(armed) if is_warm_result_stale(armed, decision.tokens.unwrap_or(0.0), decision.settings.keep_recent_tokens as f64) => IdleWarmAction::Replace,
        Some(_) => IdleWarmAction::None,
    }
}
pub fn admit_context_tool_results(messages: &[Value], window: u64, enabled: bool) -> Vec<Value> {
    let mut output = messages.to_vec();
    if !enabled { return output; }
    for message in &mut output {
        if message.get("role").and_then(Value::as_str) != Some("toolResult") { continue; }
        if let Some(text) = message.get("content").and_then(Value::as_str) {
            let admitted = admit_tool_result(text, window);
            if admitted.projected { message["content"] = json!([{"type":"text","text":admitted.text}]); }
            continue;
        }
        let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else { continue; };
        let text_tokens: Vec<_> = content.iter().map(|part| if part.get("type").and_then(Value::as_str) == Some("text") { estimate_tokens(&json!({"role":"user","content":part["text"]})) } else { 0 }).collect();
        let total = text_tokens.iter().sum::<u64>();
        let cap = resolve_tool_result_admission_cap_tokens(window);
        if total <= cap { continue; }
        let preserved = text_tokens.iter().filter(|&&t| t <= cap).sum::<u64>();
        let preserve_under_cap = preserved <= cap;
        let mut remaining_budget = if preserve_under_cap { cap - preserved } else { cap };
        let mut remaining_oversized = if preserve_under_cap { text_tokens.iter().filter(|&&t| t > cap).sum::<u64>() } else { total };
        for (part, part_tokens) in content.iter_mut().zip(text_tokens) {
            if part.get("type").and_then(Value::as_str) != Some("text") || (preserve_under_cap && part_tokens <= cap) { continue; }
            let budget = remaining_budget * part_tokens / remaining_oversized.max(1);
            let admitted = admit_tool_result_within_budget(part.get("text").and_then(Value::as_str).unwrap_or_default(), budget);
            remaining_oversized -= part_tokens;
            remaining_budget -= estimate_tokens(&json!({"role":"user","content":admitted.text}));
            if admitted.projected { part["text"] = json!(admitted.text); }
        }
    }
    output
}
