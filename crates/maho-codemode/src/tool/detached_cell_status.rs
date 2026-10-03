use crate::extension::wake_source_state::{SENPI_CODEMODE_WAKE_SOURCE, WakeSourceState, WakeSourceStateItem};
use super::{detached_cell_contract::{EvalDetachedCellState, EvalDetachedCellStatusEntry}, detached_cell_snapshot::{DetachedCellResultSource, queued_behind_cell}};

pub fn detached_status_entries(cells: &[&DetachedCellResultSource]) -> Vec<EvalDetachedCellStatusEntry> {
    cells.iter().map(|cell| EvalDetachedCellStatusEntry { cell_id:cell.cell_id.clone(),language:cell.input.language,summary:Some(cell.input.summary.clone()),started_at_ms:cell.run_started_at_ms.unwrap_or(cell.started_at_ms),queued_behind:if cell.state == EvalDetachedCellState::Queued { queued_behind_cell(cell) } else { None } }).collect()
}

pub fn detached_wake_source_state(cells: &[&DetachedCellResultSource]) -> WakeSourceState {
    WakeSourceState { source:SENPI_CODEMODE_WAKE_SOURCE.into(),active_count:cells.len(),items:Some(cells.iter().map(|cell| WakeSourceStateItem { id:cell.cell_id.clone(),description:if cell.input.summary.is_empty() { cell.cell_id.clone() } else { cell.input.summary.clone() },started_at_ms:cell.started_at_ms }).collect()) }
}
