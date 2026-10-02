//! Task-intent anchors survive local summarization but not remote checkpoints.
use serde_json::Value;
const TASK_INTENT_OPEN: &str = "<task-intent>";
const TASK_INTENT_CLOSE: &str = "</task-intent>";
const TASK_INTENT_BYTE_CAP: usize = 4096;

#[derive(Debug, PartialEq, Eq)]
pub struct ExtractedTaskIntent {
    pub task_intent: Option<String>,
    pub summary_text: String,
}
fn find_block<'a>(text: &'a str, open: &str, close: &str) -> Option<(usize, usize, &'a str)> {
    let start = text.find(open)?;
    let body_start = start + open.len();
    let end = body_start + text.get(body_start..)?.find(close)?;
    Some((start, end + close.len(), text.get(body_start..end)?))
}
pub fn cap_utf8_bytes(text: &str, max_bytes: usize) -> &str {
    let end = text.char_indices().take_while(|(index, ch)| index + ch.len_utf8() <= max_bytes)
        .last().map_or(0, |(index, ch)| index + ch.len_utf8());
    text.get(..end).unwrap_or_default().trim()
}
pub fn extract_task_intent(response_text: &str) -> ExtractedTaskIntent {
    let mut task_intent = None;
    let mut remaining = response_text.to_owned();
    while let Some((start, end, inner)) = find_block(&remaining, TASK_INTENT_OPEN, TASK_INTENT_CLOSE) {
        if task_intent.is_none() { task_intent = Some(cap_utf8_bytes(inner.trim(), TASK_INTENT_BYTE_CAP).to_owned()); }
        remaining.replace_range(start..end, "");
    }
    let mut summaries = Vec::new();
    let mut search = remaining.as_str();
    while let Some((_, end, inner)) = find_block(search, "<summary>", "</summary>") {
        summaries.push(inner.trim());
        search = search.get(end..).unwrap_or_default();
    }
    let summary_text = if summaries.is_empty() { remaining.trim().to_owned() } else { summaries.join("\n").trim().to_owned() };
    ExtractedTaskIntent { task_intent, summary_text }
}
pub fn sanitize_task_intent(text: &str) -> String {
    text.replace(TASK_INTENT_CLOSE, "[/task-intent]")
}
pub fn resolve_inherited_task_intent(branch_entries: &[Value]) -> Option<String> {
    for entry in branch_entries.iter().rev() {
        let Some(entry) = entry.as_object() else { continue; };
        let Some(details) = entry.get("details").and_then(Value::as_object) else { continue; };
        let schema = details.get("schema").and_then(Value::as_str).or_else(|| entry.get("schema").and_then(Value::as_str));
        if schema != Some("senpi.compaction.summary.v1") { continue; }
        let Some(intent) = details.get("taskIntent").and_then(Value::as_str) else { continue; };
        if !intent.trim().is_empty() { return Some(cap_utf8_bytes(intent.trim(), TASK_INTENT_BYTE_CAP).to_owned()); }
    }
    None
}
