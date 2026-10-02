use std::{collections::HashMap, path::PathBuf, sync::{Arc, Mutex}};
use maho_ext_api::AgentToolResult;
use crate::{config::settings::{DEFAULT_HARD_LIMIT_SECONDS, DEFAULT_RUN_BUDGET_SECONDS, DEFAULT_MAX_DETACHED_CELLS}, extension::wake_source_state::WakeSourceState, timeouts::idle_timeout::TimeoutPauseHandle};
use super::{types::{EvalToolInput, EvalLanguage}, managed_cell::{ManagedCell, create_managed_cell}, detached_cell_contract::{EvalDetachedCellSnapshot, EvalDetachedCellState, EvalDetachedCellStatusEntry}, detached_cell_snapshot::{LiveResultProvider, QueueSnapshotProvider, snapshot_detached_cell, current_detached_result, detached_error_result}, detached_cell_state::{detached_cell_is_active, allows_detached_cell_transition, active_detached_cell_reuse_error}, detached_cell_status::{detached_status_entries, detached_wake_source_state}, terminal_snapshot_store::TerminalSnapshotStore, detached_notification_queue::{DetachedNotificationQueue, DetachedNotifier, PendingDetachedNotification}};

pub type ManagedCellHandle = Arc<Mutex<ManagedCell>>;
pub type DetachedStatusCallback = Arc<dyn Fn(Vec<EvalDetachedCellStatusEntry>) + Send + Sync>;
pub type DetachedWakeCallback = Arc<dyn Fn(WakeSourceState) + Send + Sync>;

pub struct DetachedCellManagerOptions {
    pub artifacts_dir: Option<PathBuf>,
    pub on_status_change: Option<DetachedStatusCallback>,
    pub on_wake_source_state: Option<DetachedWakeCallback>,
    pub notifier: Option<DetachedNotifier>,
    pub now: Arc<dyn Fn() -> f64 + Send + Sync>,
    pub hard_limit_seconds: f64,
    pub run_budget_seconds: f64,
    pub max_detached_cells: usize,
}

impl Default for DetachedCellManagerOptions {
    fn default() -> Self {
        Self { artifacts_dir:None, on_status_change:None, on_wake_source_state:None, notifier:None,
            now:Arc::new(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("system clock").as_secs_f64() * 1000.0),
            hard_limit_seconds:DEFAULT_HARD_LIMIT_SECONDS, run_budget_seconds:DEFAULT_RUN_BUDGET_SECONDS, max_detached_cells:DEFAULT_MAX_DETACHED_CELLS as usize }
    }
}

pub struct EvalDetachedCellManager {
    options: DetachedCellManagerOptions,
    cells: HashMap<String, ManagedCellHandle>,
    detached: HashMap<String, ManagedCellHandle>,
    terminal_snapshots: TerminalSnapshotStore,
    notification_queue: DetachedNotificationQueue,
}

impl EvalDetachedCellManager {
    pub fn new(mut options: DetachedCellManagerOptions) -> Self {
        let notification_queue = DetachedNotificationQueue::new(options.notifier.take());
        Self { options, cells:HashMap::new(), detached:HashMap::new(), terminal_snapshots:TerminalSnapshotStore::default(), notification_queue }
    }

    pub fn max_detached_cells(&self) -> usize { self.options.max_detached_cells }

    pub fn create(&mut self, cell_id: String, input: EvalToolInput) -> Result<ManagedCellHandle, String> {
        if let Some(existing) = self.cells.get(&cell_id) {
            let existing = existing.lock().expect("managed cell poisoned");
            if detached_cell_is_active(existing.source.state) {
                return Err(active_detached_cell_reuse_error(&cell_id, existing.source.input.language, existing.source.state, existing.source.detached));
            }
        }
        self.cells.remove(&cell_id);
        self.terminal_snapshots.delete(&cell_id);
        let cell = Arc::new(Mutex::new(create_managed_cell(cell_id.clone(), input, self.options.artifacts_dir.as_deref(), (self.options.now)(), self.options.hard_limit_seconds, self.options.run_budget_seconds)));
        self.cells.insert(cell_id, cell.clone());
        Ok(cell)
    }

    pub fn bind_kernel(&self, cell: &ManagedCellHandle, live_result: LiveResultProvider, queue_snapshot: QueueSnapshotProvider) {
        let mut cell = cell.lock().expect("managed cell poisoned");
        if !detached_cell_is_active(cell.source.state) { return; }
        cell.source.live_result = Some(live_result);
        cell.source.queue_snapshot = Some(queue_snapshot);
        cell.can_detach = true;
    }

    pub fn mark_running(&self, cell: &ManagedCellHandle) {
        let mut cell = cell.lock().expect("managed cell poisoned");
        if !allows_detached_cell_transition(cell.source.state, EvalDetachedCellState::Running) { return; }
        cell.source.state = EvalDetachedCellState::Running;
        cell.source.run_started_at_ms = Some((self.options.now)());
        cell.deadlines.resume();
        let detached = cell.source.detached;
        drop(cell);
        if detached { self.emit_status(); }
    }

    pub fn pause(&self, cell: &ManagedCellHandle) { cell.lock().expect("managed cell poisoned").deadlines.pause(); }
    pub fn resume(&self, cell: &ManagedCellHandle) { cell.lock().expect("managed cell poisoned").deadlines.resume(); }

    pub fn detach(&mut self, cell: &ManagedCellHandle) -> bool {
        let mut managed = cell.lock().expect("managed cell poisoned");
        if !managed.can_detach || !detached_cell_is_active(managed.source.state) || managed.source.detached || self.detached.len() >= self.options.max_detached_cells { return false; }
        managed.source.detached = true;
        managed.was_detached = true;
        self.detached.insert(managed.source.cell_id.clone(), cell.clone());
        drop(managed);
        self.emit_status();
        true
    }

    pub fn complete(&mut self, cell: &ManagedCellHandle, result: AgentToolResult) -> bool {
        let state = if result.details["cells"][0]["status"] == "cancelled" { EvalDetachedCellState::Cancelled }
            else if result.details["isError"] == true { EvalDetachedCellState::Failed } else { EvalDetachedCellState::Completed };
        self.settle(cell, state, result)
    }

    pub fn fail(&mut self, cell: &ManagedCellHandle, error: &str) -> bool {
        let result = detached_error_result(&cell.lock().expect("managed cell poisoned").source, error, None);
        self.settle(cell, EvalDetachedCellState::Failed, result)
    }

    fn settle(&mut self, cell: &ManagedCellHandle, state: EvalDetachedCellState, result: AgentToolResult) -> bool {
        let mut managed = cell.lock().expect("managed cell poisoned");
        if !allows_detached_cell_transition(managed.source.state, state) { return false; }
        managed.deadlines.clear();
        managed.source.state = state;
        managed.source.terminal_result = Some(result);
        managed.source.live_result = None;
        let snapshot = snapshot_detached_cell(&managed.source, (self.options.now)());
        managed.terminal.send_replace(Some(snapshot.clone()));
        self.cells.remove(&managed.source.cell_id);
        self.terminal_snapshots.remember(snapshot);
        let detached = managed.was_detached;
        if detached {
            self.detached.remove(&managed.source.cell_id);
            if !managed.notification_queued {
                managed.notification_queued = true;
                let cell = cell.clone();
                let now = self.options.now.clone();
                self.notification_queue.enqueue(PendingDetachedNotification {
                    spill_path:managed.spill_path.clone(), snapshot:Box::new(move || Box::pin(async move {
                        let outcome = cell.lock().expect("managed cell poisoned").interrupt_outcome.clone();
                        if let Some(mut outcome) = outcome { let _ = outcome.wait_for(|settled| *settled).await; }
                        snapshot_detached_cell(&cell.lock().expect("managed cell poisoned").source, now())
                    })),
                });
            }
        }
        drop(managed);
        if detached { self.emit_status(); }
        true
    }

    pub fn peek(&self, cell_id: &str) -> Result<EvalDetachedCellSnapshot, String> {
        if let Some(cell) = self.cells.get(cell_id) { return Ok(snapshot_detached_cell(&cell.lock().expect("managed cell poisoned").source, (self.options.now)())); }
        self.terminal_snapshots.get(cell_id).cloned().ok_or_else(|| format!("Unknown detached eval cell \"{cell_id}\""))
    }

    pub fn live_cells(&self, language: Option<EvalLanguage>, except: Option<&str>) -> Vec<EvalDetachedCellSnapshot> {
        let mut live: Vec<_> = self.cells.values().filter_map(|cell| {
            let cell = cell.lock().expect("managed cell poisoned");
            (detached_cell_is_active(cell.source.state) && language.is_none_or(|language| cell.source.input.language == language) && except != Some(cell.source.cell_id.as_str())).then(|| snapshot_detached_cell(&cell.source, (self.options.now)()))
        }).collect();
        live.sort_by(|left, right| left.started_at_ms.total_cmp(&right.started_at_ms));
        live
    }

    pub fn list(&self) -> (Vec<EvalDetachedCellSnapshot>, Vec<EvalDetachedCellSnapshot>) {
        (self.live_cells(None, None), self.terminal_snapshots.list().into_iter().cloned().collect())
    }

    pub fn terminal_signal(&self, cell_id: &str) -> Result<tokio::sync::watch::Receiver<Option<EvalDetachedCellSnapshot>>, String> {
        if let Some(cell) = self.cells.get(cell_id) { return Ok(cell.lock().expect("managed cell poisoned").terminal.subscribe()); }
        let (_, receiver) = tokio::sync::watch::channel(Some(self.peek(cell_id)?));
        Ok(receiver)
    }

    pub async fn flush_notifications(&self) -> Result<(), String> { self.notification_queue.flush().await }

    pub fn cancel_without_interrupt(&mut self, cell: &ManagedCellHandle) -> bool {
        let result = current_detached_result(&cell.lock().expect("managed cell poisoned").source);
        self.settle(cell, EvalDetachedCellState::Cancelled, result)
    }

    fn emit_status(&self) {
        let cells: Vec<_> = self.detached.values().map(|cell| cell.lock().expect("managed cell poisoned")).collect();
        let sources: Vec<_> = cells.iter().map(|cell| &cell.source).collect();
        let status = detached_status_entries(&sources);
        let wake = detached_wake_source_state(&sources);
        drop(cells);
        if let Some(callback) = &self.options.on_status_change { callback(status); }
        if let Some(callback) = &self.options.on_wake_source_state { callback(wake); }
    }

    pub fn publish_wake_source_state(&self) {
        let cells: Vec<_> = self.detached.values().map(|cell| cell.lock().expect("managed cell poisoned")).collect();
        let sources: Vec<_> = cells.iter().map(|cell| &cell.source).collect();
        let wake = detached_wake_source_state(&sources);
        drop(cells);
        if let Some(callback) = &self.options.on_wake_source_state { callback(wake); }
    }
}
