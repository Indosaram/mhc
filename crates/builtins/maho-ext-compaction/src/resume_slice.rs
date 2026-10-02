use maho_core::{compaction::compaction::{estimate_tokens,find_cut_point},session_manager::build_session_context};
use serde_json::{Value,json};
use crate::model_usability_budget::ModelUsabilityBudgetProjection;
pub const RESUME_SLICE_SCHEMA: &str = "senpi.compaction.resume-slice.v1";
pub const RESUME_SLICE_ORIGIN: &str = "resume-admission";
const PREVIEW_ENTRY_ID: &str = "__senpi_resume_slice_preview__";
const CHECKPOINT_MARKER: &str = "[Resume recovery checkpoint]\nThe restored conversation was larger than this model's context window, so older context was reduced without any provider request.\nThe complete transcript is still recorded in the session file. Continue from the retained messages and treat omitted details as unknown.";
#[derive(Clone, Debug, PartialEq)]
pub struct ResumeSlicePlan { pub first_kept_entry_id: String, pub summary: String, pub tokens_before: f64, pub tokens_after: f64, pub dropped_entries: usize }
pub fn resume_slice_notice(plan: &ResumeSlicePlan) -> String {
    format!("Restored context of {} tokens exceeded this model's window, so older context was reduced to {} tokens before the first prompt. The full transcript is preserved in the session file.",plan.tokens_before,plan.tokens_after)
}
pub fn plan_resume_slice(entries: &[Value], projection: &ModelUsabilityBudgetProjection) -> Option<ResumeSlicePlan> {
    let overhead = projection.system_prompt_tokens + projection.active_tool_schema_tokens + projection.output_reserve_tokens + projection.compaction_reserve_tokens + projection.speculation_lead_tokens + projection.safety_margin_tokens;
    let mut target = projection.context_window - overhead;
    if target < 1024.0 { return None; }
    let previous = entries.iter().enumerate().rev().find(|(_,e)|e.get("type").and_then(Value::as_str)==Some("compaction"));
    let boundary = previous.map_or(0,|(i,e)|entries.iter().position(|candidate|candidate.get("id")==e.get("firstKeptEntryId")).unwrap_or(i+1));
    let carried = previous.and_then(|(_,e)|e.get("summary").and_then(Value::as_str)).map(str::trim).filter(|s|!s.is_empty());
    let summary = if let Some(carried) = carried {
        let note = "\n[Earlier checkpoint truncated]";
        let bounded = if carried.encode_utf16().count() <= 8000 {carried.into()} else {format!("{}{note}",String::from_utf16_lossy(&carried.encode_utf16().take(8000-note.encode_utf16().count()).collect::<Vec<_>>()))};
        format!("{CHECKPOINT_MARKER}\n\nEarlier checkpoint:\n{bounded}")
    } else {CHECKPOINT_MARKER.into()};
    let mut measured = None;
    while target >= 1024.0 {
        let cut = find_cut_point(entries,boundary,entries.len(),target as i64);
        if measured != Some(cut.first_kept_entry_index) {
            measured = Some(cut.first_kept_entry_index);
            if let Some(id) = entries.get(cut.first_kept_entry_index).and_then(|e|e.get("id")).and_then(Value::as_str).filter(|id|!id.is_empty()) {
                let mut preview = entries.to_vec();
                preview.push(json!({"type":"compaction","id":PREVIEW_ENTRY_ID,"parentId":entries.last().and_then(|e|e.get("id")),"timestamp":"1970-01-01T00:00:00.000Z","summary":summary,"firstKeptEntryId":id,"tokensBefore":projection.live_context_tokens,"fromHook":false}));
                let after = build_session_context(&preview,Some(PREVIEW_ENTRY_ID)).messages.iter().map(estimate_tokens).sum::<u64>() as f64;
                if after + overhead <= projection.context_window { return Some(ResumeSlicePlan {first_kept_entry_id:id.into(),summary,tokens_before:projection.live_context_tokens,tokens_after:after,dropped_entries:cut.first_kept_entry_index.saturating_sub(boundary)}); }
            }
        }
        target = (target/2.0).floor();
    }
    None
}
