use maho_ext_api::{AgentToolResult, ContentBlock};
use serde_json::{Value, json};
use super::{detached_cell_contract::{EvalDetachedCellSnapshot, EvalDetachedCellState}, interrupt_note::interruption_state_note};

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
