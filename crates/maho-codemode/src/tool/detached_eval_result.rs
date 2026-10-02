use maho_ext_api::{AgentToolResult, ContentBlock};
use serde_json::{Value, json};
use super::{detached_cell_contract::{EvalDetachedCellSnapshot, EvalDetachedCellState}, interrupt_note::interruption_state_note};

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct EvalBackgroundCapacityError { pub code: &'static str, pub message: String }

impl EvalBackgroundCapacityError {
    pub fn new(cap: usize, cell_id: &str, foreground_ms: f64, live_cell_ids: &[String]) -> Self {
        Self { code: "eval_background_capacity_reached", message: format!("Background capacity ({cap}) reached: cell {cell_id} ran {}s in the foreground and was cancelled when the foreground window elapsed. Live cells: {}. Stop one with eval({{ action: \"stop\", cell_id }}) or wait for a notification, then re-run this step.", (foreground_ms / 1000.0).floor(), live_cell_ids.join(", ")) }
    }
}

pub fn result_after_detach(snapshot: &EvalDetachedCellSnapshot, input: &super::types::EvalToolInput, other_live_cells: usize) -> AgentToolResult {
    if !matches!(snapshot.state, EvalDetachedCellState::Detached | EvalDetachedCellState::Running) { return create_detached_control_result(snapshot); }
    let language = match input.language { super::types::EvalLanguage::Js => "js", super::types::EvalLanguage::Py => "py", super::types::EvalLanguage::Rb => "rb", super::types::EvalLanguage::Jl => "jl" };
    let text = match &snapshot.queued_behind {
        None => format!("Eval cell {} detached and is running in the {language} kernel ({other_live_cells} other live cells). Completion arrives as a notification; do not re-run it. eval({{ action: \"peek\" | \"stop\", cell_id }}) or eval({{ action: \"list\" }}).", snapshot.cell_id),
        Some(predecessors) => { let predecessors = predecessors.join(", "); format!("Eval cell {} is queued behind {predecessors} in the {language} kernel and detached; it runs after {predecessors} and completes as one notification. peek/stop/list with eval({{ action, cell_id }})", snapshot.cell_id) }
    };
    let mut result = AgentToolResult::text(text);
    result.details = snapshot.result.details.clone();
    result
}

pub fn result_for_detached_state(result: &AgentToolResult, state: EvalDetachedCellState, duration_ms: f64, queued_behind: Option<&[String]>) -> AgentToolResult {
    let mut result = result.clone();
    let terminal = matches!(state, EvalDetachedCellState::Completed | EvalDetachedCellState::Failed | EvalDetachedCellState::Cancelled);
    if !terminal || result.details.get("durationMs").is_none() { result.details["durationMs"] = json!(duration_ms); }
    if let Some(cells) = result.details["cells"].as_array_mut() && let Some(cell) = cells.first_mut() {
        if !terminal || cell.get("durationMs").is_none() { cell["durationMs"] = json!(duration_ms); }
        if let Some(object) = cell.as_object_mut() { object.remove("queuedBehind"); }
        cell["status"] = Value::String(match state {
            EvalDetachedCellState::Detached if queued_behind.is_some() => "queued",
            EvalDetachedCellState::Queued => "queued",
            EvalDetachedCellState::Running => "running",
            EvalDetachedCellState::Detached => "detached",
            EvalDetachedCellState::Completed => "complete",
            EvalDetachedCellState::Failed => "error",
            EvalDetachedCellState::Cancelled => "cancelled",
        }.into());
        if let Some(queued) = queued_behind { cell["queuedBehind"] = json!(queued); }
    }
    result
}

pub fn create_detached_control_result(snapshot: &EvalDetachedCellSnapshot) -> AgentToolResult {
    let language = match snapshot.language { super::types::EvalLanguage::Js => "js", super::types::EvalLanguage::Py => "py", super::types::EvalLanguage::Rb => "rb", super::types::EvalLanguage::Jl => "jl" };
    let output = snapshot.result.content.iter().filter_map(|part| match part { ContentBlock::Text(text) => Some(text.text.as_str()), _ => None }).collect::<Vec<_>>().join("\n");
    let mut lines = vec![format!("Eval cell {} ({language}) is {}.", snapshot.cell_id, snapshot.state.as_str()), if output.is_empty() { "(no buffered output)".into() } else { output }];
    if snapshot.state == EvalDetachedCellState::Cancelled && let Some(note) = interruption_state_note(snapshot.language, snapshot.state_retained) { lines.push(note); }
    if let Some(note) = &snapshot.interrupt_note { lines.push(note.trim().into()); }
    let mut result = AgentToolResult::text(lines.join("\n"));
    result.content.extend(snapshot.result.content.iter().filter(|part| matches!(part, ContentBlock::Image(_))).cloned());
    result.details = snapshot.result.details.clone();
    if snapshot.state == EvalDetachedCellState::Failed { result.details["isError"] = json!(true); }
    result
}
