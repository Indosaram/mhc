use std::path::{Path, PathBuf};
use maho_ext_api::ContentBlock;
use super::{detached_cell_contract::{EvalDetachedCellSnapshot, EvalDetachedCellState}, interrupt_note::{interruption_state_note, unknown_interruption_state_note}, types::EvalLanguage};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvalDetachedCellNotification { pub cell_id: String, pub content: String }

pub fn detached_notification_spill_path(artifacts_dir: Option<&Path>, cell_id: &str) -> Option<PathBuf> {
    let safe = cell_id.chars().map(|character| if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') { character } else { '_' }).collect::<String>();
    artifacts_dir.map(|root| root.join("local").join(format!("detached-eval-{safe}.log")))
}

pub async fn build_detached_cell_notification(snapshot: &EvalDetachedCellSnapshot, spill_path: Option<&Path>) -> EvalDetachedCellNotification {
    let language = match snapshot.language { EvalLanguage::Js => "js", EvalLanguage::Py => "py", EvalLanguage::Rb => "rb", EvalLanguage::Jl => "jl" };
    let outcome = if let Some(seconds) = snapshot.hard_limit_seconds { format!("was killed at the {seconds}s hard limit") }
        else if let Some(seconds) = snapshot.run_budget_seconds { format!("was killed after exhausting its {seconds}s run budget (own execution time; host tool calls excluded)") }
        else { match snapshot.state { EvalDetachedCellState::Completed => "completed", EvalDetachedCellState::Cancelled => "cancelled", _ => "failed" }.into() };
    let text = snapshot.result.content.iter().filter_map(|part| match part { ContentBlock::Text(text) => Some(text.text.as_str()), _ => None }).collect::<Vec<_>>().join("\n");
    let text = if text.is_empty() { snapshot.output_tail.as_str() } else { text.as_str() };
    let note = if snapshot.state != EvalDetachedCellState::Cancelled {
        match snapshot.result.details.get("kernelState").and_then(serde_json::Value::as_str) {
            Some("lost") => "The kernel died while this cell ran - every global is lost; the next eval cell runs on a fresh kernel.".into(),
            Some("restarted") => "The kernel was restarted before this cell ran - globals from earlier cells are gone; this cell's variables are available to the next eval cell.".into(),
            Some("not-run") => "This cell never ran and changed no kernel state.".into(),
            _ => memory_state_note(snapshot.result.details.get("memory")),
        }
    }
        else {
            let note = interruption_state_note(snapshot.language, snapshot.state_retained).unwrap_or_else(|| unknown_interruption_state_note(snapshot.language));
            if let Some(extra) = &snapshot.interrupt_note { format!("{note} {}", extra.trim()) } else { note }
        };
    let header = format!("<system-reminder>Detached eval cell {} ({language}) {outcome}.", snapshot.cell_id);
    let body = format!("{header}\n{}\n{note}</system-reminder>", if text.is_empty() { "(no output)" } else { text });
    let overflow = body.len() > 512;
    let mut spill_notice = String::new();
    if overflow && let Some(path) = spill_path {
        let spill = async {
            if let Some(parent) = path.parent() { tokio::fs::create_dir_all(parent).await?; }
            tokio::fs::write(path, &body).await
        }.await;
        spill_notice = match spill {
            Ok(()) => format!("\nBuffered output overflowed; full output: {}", path.display()),
            Err(error) => format!("\nBuffered output overflow could not be spilled: {error}"),
        };
    }
    let content = if overflow {
        let mut start = text.len().saturating_sub(512);
        while !text.is_char_boundary(start) { start += 1; }
        format!("{header}\nBuffered output tail:\n{}\n{note}</system-reminder>\n[…notification tail capped…]{spill_notice}", &text[start..])
    } else { body };
    EvalDetachedCellNotification { cell_id: snapshot.cell_id.clone(), content }
}

fn memory_state_note(memory: Option<&serde_json::Value>) -> String {
    if memory.is_some_and(|state| state["overCeiling"] == true) {
        "Kernel memory is over its ceiling - the kernel restarts before the next eval cell and every global is lost.".into()
    } else if memory.is_some_and(|state| state["recycled"] == true) {
        "The kernel was restarted before this cell ran - globals from earlier cells are gone; this cell's variables are available to the next eval cell.".into()
    } else { "Kernel state updated - variables are available to the next eval cell.".into() }
}

#[cfg(test)]
mod memory_note_tests {
    use super::*;

    #[test]
    fn reports_kernel_and_memory_lifecycle_states() {
        assert!(memory_state_note(Some(&serde_json::json!({"overCeiling":true}))).contains("every global is lost"));
        assert!(memory_state_note(Some(&serde_json::json!({"recycled":true}))).contains("restarted before this cell ran"));
        assert!(memory_state_note(None).contains("Kernel state updated"));
    }
}
