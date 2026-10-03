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
    let note = if snapshot.state != EvalDetachedCellState::Cancelled { "Kernel state updated - variables are available to the next eval cell.".into() }
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
