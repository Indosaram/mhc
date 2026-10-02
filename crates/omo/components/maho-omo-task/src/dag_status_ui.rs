use std::{collections::BTreeMap, sync::{Arc, Mutex, PoisonError}};
use maho_ext_api::{ExtensionMode, ExtensionWidgetOptions, WidgetContent, WidgetPlacement};
use senpi_task::dag::{manager::DagRunSummary, types::DagRunSnapshot};
use crate::{dag_status_row_format::run_rows, runtime_context::TaskRuntimeContext, status_ui::StatusUiTimers};

pub const DAG_STATUS_UI_KEY: &str = "omo-dag";
pub trait DagStatusUiManager: Send + Sync {
    fn list(&self, session: &str) -> Vec<DagRunSummary>;
    fn snapshot(&self, run: &str, session: &str) -> Option<DagRunSnapshot>;
}
#[derive(Default)]
struct State { activity: BTreeMap<String, BTreeMap<String, String>>, pending: Option<u64>, live_refresh: Option<u64> }
pub struct DagStatusUi {
    manager: Arc<dyn DagStatusUiManager>,
    runtime: Arc<Mutex<TaskRuntimeContext>>,
    timers: Arc<dyn StatusUiTimers>,
    state: Mutex<State>,
}
impl DagStatusUi {
    pub fn new(manager: Arc<dyn DagStatusUiManager>, runtime: Arc<Mutex<TaskRuntimeContext>>, timers: Arc<dyn StatusUiTimers>) -> Arc<Self> { Arc::new(Self { manager, runtime, timers, state: Mutex::new(State::default()) }) }
    pub fn sync_now(self: &Arc<Self>) {
        let (ui, session) = {
            let runtime = self.runtime.lock().unwrap_or_else(PoisonError::into_inner);
            if runtime.mode() != Some(ExtensionMode::Tui) { drop(runtime); self.clear_live_refresh(); return }
            (runtime.ui().cloned(), runtime.session_id().map(str::to_owned))
        };
        let Some(ui) = ui else { self.clear_live_refresh(); return };
        let runs: Vec<_> = session.map_or_else(Vec::new, |session| self.manager.list(&session).into_iter().filter(|run| !run.status.is_terminal()).filter_map(|run| self.manager.snapshot(&run.run_id, &session)).collect());
        let rows = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let rows: Vec<_> = runs.iter().flat_map(|run| run_rows(run, state.activity.get(&run.run_id))).collect();
            state.activity.retain(|id, _| runs.iter().any(|run| &run.run_id == id));
            rows
        };
        if rows.is_empty() { self.clear_live_refresh(); ui.set_widget(DAG_STATUS_UI_KEY, None, ExtensionWidgetOptions::default()); return }
        ui.set_widget(DAG_STATUS_UI_KEY, Some(WidgetContent::Lines(rows)), ExtensionWidgetOptions { placement: WidgetPlacement::BelowEditor });
        if runs.iter().any(|run| !run.status.is_terminal()) { self.schedule_live_refresh(); } else { self.clear_live_refresh(); }
    }
    pub fn on_activity(self: &Arc<Self>, run: &str, node: &str, activity: &str) {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).activity.entry(run.into()).or_default().insert(node.into(), activity.into());
        self.schedule_sync();
    }
    pub fn schedule_sync(self: &Arc<Self>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.live_refresh.is_some() { return }
        if let Some(handle) = state.pending.take() { self.timers.clear(handle); }
        let weak = Arc::downgrade(self);
        state.pending = Some(self.timers.set(Box::new(move || { if let Some(ui) = weak.upgrade() { ui.state.lock().unwrap_or_else(PoisonError::into_inner).pending = None; ui.sync_now(); } }), 250));
    }
    fn schedule_live_refresh(self: &Arc<Self>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.live_refresh.is_some() { return }
        let weak = Arc::downgrade(self);
        state.live_refresh = Some(self.timers.set(Box::new(move || { if let Some(ui) = weak.upgrade() { ui.state.lock().unwrap_or_else(PoisonError::into_inner).live_refresh = None; ui.sync_now(); } }), 1000));
    }
    fn clear_live_refresh(&self) { if let Some(handle) = self.state.lock().unwrap_or_else(PoisonError::into_inner).live_refresh.take() { self.timers.clear(handle); } }
    pub fn dispose(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(handle) = state.pending.take() { self.timers.clear(handle); }
        if let Some(handle) = state.live_refresh.take() { self.timers.clear(handle); }
        state.activity.clear();
    }
}
