use std::sync::Arc;
use maho_ext_api::{AgentToolResult, ContentBlock};
use serde_json::json;
use super::{detached_cell_contract::{EvalDetachedCellSnapshot, EvalDetachedCellState}, detached_eval_result::result_for_detached_state, types::EvalToolInput};

pub type LiveResultProvider = Arc<dyn Fn() -> AgentToolResult + Send + Sync>;
pub type QueueSnapshotProvider = Arc<dyn Fn() -> (Option<String>, Vec<String>) + Send + Sync>;

pub struct DetachedCellResultSource {
    pub cell_id: String,
    pub input: EvalToolInput,
    pub started_at_ms: f64,
    pub run_started_at_ms: Option<f64>,
    pub detached: bool,
    pub state: EvalDetachedCellState,
    pub queue_snapshot: Option<QueueSnapshotProvider>,
    pub state_retained: Option<bool>,
    pub interrupt_note: Option<String>,
    pub live_result: Option<LiveResultProvider>,
    pub terminal_result: Option<AgentToolResult>,
    pub hard_limited: bool,
    pub hard_limit_seconds: Option<f64>,
    pub run_budget_exhausted: bool,
    pub run_budget_seconds: Option<f64>,
}

pub fn queued_behind_cell(cell: &DetachedCellResultSource) -> Option<Vec<String>> {
    if cell.state != EvalDetachedCellState::Queued { return None; }
    let Some(provider) = &cell.queue_snapshot else { return Some(vec![]); };
    let (active, queued) = provider();
    let index = queued.iter().position(|id| id == &cell.cell_id).unwrap_or(queued.len());
    Some(active.filter(|id| id != &cell.cell_id).into_iter().chain(queued.into_iter().take(index)).collect())
}

pub fn current_detached_result(cell: &DetachedCellResultSource) -> AgentToolResult {
    if let Some(result) = &cell.terminal_result { return result.clone(); }
    if let Some(provider) = &cell.live_result { return provider(); }
    let mut result = AgentToolResult::text("(no output)");
    result.details = json!({"language":cell.input.language,"languages":[cell.input.language],"summary":cell.input.summary,"durationMs":0,"toolCalls":[],"truncated":false,"cells":[{"index":0,"summary":cell.input.summary,"code":cell.input.code,"language":cell.input.language,"output":"","status":"running","durationMs":0}]});
    result
}

fn output_tail(result: &AgentToolResult) -> String {
    if let Some(output) = result.details["cells"][0]["output"].as_str() { return output.into(); }
    result.content.iter().filter_map(|part| match part { ContentBlock::Text(text) => Some(text.text.as_str()), _ => None }).collect::<Vec<_>>().join("\n")
}

pub fn snapshot_detached_cell(cell: &DetachedCellResultSource, now_ms: f64) -> EvalDetachedCellSnapshot {
    let duration_ms = cell.run_started_at_ms.map_or(0.0, |started| (now_ms - started).max(0.0));
    let state = if cell.detached && matches!(cell.state, EvalDetachedCellState::Queued | EvalDetachedCellState::Running) { EvalDetachedCellState::Detached } else { cell.state };
    let queued_behind = queued_behind_cell(cell);
    let result = result_for_detached_state(&current_detached_result(cell), state, duration_ms, queued_behind.as_deref());
    EvalDetachedCellSnapshot { cell_id:cell.cell_id.clone(),language:cell.input.language,started_at_ms:cell.started_at_ms,state,queued_behind,output_tail:output_tail(&result),result,state_retained:cell.state_retained,interrupt_note:cell.interrupt_note.clone(),hard_limit_seconds:cell.hard_limited.then_some(cell.hard_limit_seconds).flatten(),run_budget_seconds:cell.run_budget_exhausted.then_some(cell.run_budget_seconds).flatten() }
}

pub fn detached_error_result(cell: &DetachedCellResultSource, message: &str, code: Option<&str>) -> AgentToolResult {
    let current = current_detached_result(cell);
    let output = output_tail(&current);
    let mut result = AgentToolResult::text(if output.is_empty() { message.into() } else { format!("{output}\n{message}") });
    result.content.extend(current.content.into_iter().filter(|part| matches!(part, ContentBlock::Image(_))));
    result.details = current.details;
    result.details["isError"] = json!(true);
    if let Some(code) = code { result.details["code"] = json!(code); }
    result
}
