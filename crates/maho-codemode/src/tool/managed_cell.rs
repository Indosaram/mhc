use std::path::{Path, PathBuf};
use tokio::sync::watch;
use super::{cell_deadlines::CellDeadlines, detached_cell_contract::{EvalDetachedCellSnapshot, EvalDetachedCellState}, detached_cell_notification::detached_notification_spill_path, detached_cell_snapshot::DetachedCellResultSource, types::EvalToolInput};
use crate::timeouts::idle_timeout::TimeoutPauseHandle;

pub struct ManagedCell {
    pub source: DetachedCellResultSource,
    pub spill_path: Option<PathBuf>,
    pub can_detach: bool,
    pub was_detached: bool,
    pub notification_queued: bool,
    pub deadlines: CellDeadlines,
    pub terminal: watch::Sender<Option<EvalDetachedCellSnapshot>>,
    pub interrupt_outcome: Option<watch::Receiver<bool>>,
    pub kernel: Option<std::sync::Arc<dyn super::types::EvalKernel>>,
    pub on_kill: Option<std::sync::Arc<dyn Fn(String) + Send + Sync>>,
}

impl ManagedCell {
    pub fn record_deadline_expiry(&mut self, kind: super::cell_deadlines::CellDeadlineKind) {
        if !super::detached_cell_state::detached_cell_is_active(self.source.state) {return;}
        self.source.hard_limited=kind==super::cell_deadlines::CellDeadlineKind::HardLimit;
        self.source.run_budget_exhausted=kind==super::cell_deadlines::CellDeadlineKind::RunBudget;
    }
}

pub fn create_managed_cell(cell_id: String, input: EvalToolInput, artifacts_dir: Option<&Path>, now_ms: f64, default_hard_limit_seconds: f64, default_run_budget_seconds: f64) -> ManagedCell {
    let hard_limit_seconds = default_hard_limit_seconds.max(input.timeout.unwrap_or(0.0));
    let run_budget_seconds = input.timeout.unwrap_or(default_run_budget_seconds);
    let deadlines = CellDeadlines::new(cell_id.clone(), hard_limit_seconds, run_budget_seconds);
    deadlines.pause();
    let spill_path = detached_notification_spill_path(artifacts_dir, &cell_id);
    let (terminal, _) = watch::channel(None);
    ManagedCell { source:DetachedCellResultSource { cell_id,input,started_at_ms:now_ms,run_started_at_ms:None,detached:false,state:EvalDetachedCellState::Queued,queue_snapshot:None,state_retained:None,interrupt_note:None,live_result:None,terminal_result:None,hard_limited:false,hard_limit_seconds:Some(hard_limit_seconds),run_budget_exhausted:false,run_budget_seconds:Some(run_budget_seconds) },spill_path,can_detach:false,was_detached:false,notification_queued:false,deadlines,terminal,interrupt_outcome:None,kernel:None,on_kill:None }
}
