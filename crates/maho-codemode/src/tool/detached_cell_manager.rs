use std::{path::PathBuf, sync::{Arc, Mutex}};
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
    cells: Vec<(String, ManagedCellHandle)>,
    detached: Vec<(String, ManagedCellHandle)>,
    terminal_snapshots: TerminalSnapshotStore,
    notification_queue: DetachedNotificationQueue,
}

impl EvalDetachedCellManager {
    pub fn new(mut options: DetachedCellManagerOptions) -> Self {
        let notification_queue = DetachedNotificationQueue::new(options.notifier.take());
        Self { options, cells:Vec::new(), detached:Vec::new(), terminal_snapshots:TerminalSnapshotStore::default(), notification_queue }
    }

    pub fn max_detached_cells(&self) -> usize { self.options.max_detached_cells }

    pub fn create(&mut self, cell_id: String, input: EvalToolInput) -> Result<ManagedCellHandle, String> {
        if let Some((_,existing)) = self.cells.iter().find(|(id,_)|id==&cell_id) {
            let existing = existing.lock().expect("managed cell poisoned");
            if detached_cell_is_active(existing.source.state) {
                return Err(active_detached_cell_reuse_error(&cell_id, existing.source.input.language, existing.source.state, existing.source.detached));
            }
        }
        self.cells.retain(|(id,_)|id!=&cell_id);
        self.terminal_snapshots.delete(&cell_id);
        let cell = Arc::new(Mutex::new(create_managed_cell(cell_id.clone(), input, self.options.artifacts_dir.as_deref(), (self.options.now)(), self.options.hard_limit_seconds, self.options.run_budget_seconds)));
        self.cells.push((cell_id, cell.clone()));
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
        self.detached.push((managed.source.cell_id.clone(), cell.clone()));
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
        self.cells.retain(|(id,_)|id!=&managed.source.cell_id);
        self.terminal_snapshots.remember(snapshot);
        let detached = managed.was_detached;
        if detached {
            self.detached.retain(|(id,_)|id!=&managed.source.cell_id);
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
        if let Some((_,cell)) = self.cells.iter().find(|(id,_)|id==cell_id) { return Ok(snapshot_detached_cell(&cell.lock().expect("managed cell poisoned").source, (self.options.now)())); }
        self.terminal_snapshots.get(cell_id).cloned().ok_or_else(|| format!("Unknown detached eval cell \"{cell_id}\""))
    }

    pub async fn stop(manager: &Arc<Mutex<Self>>, cell_id: &str, reason: &str) -> Result<EvalDetachedCellSnapshot,String> {
        let cell={
            let manager=manager.lock().expect("cell manager lock");
            let Some((_,cell))=manager.cells.iter().find(|(id,_)|id==cell_id) else {return manager.peek(cell_id);};
            cell.clone()
        };
        let (kernel,queued,detached,on_kill)={let cell=cell.lock().expect("managed cell lock");(cell.kernel.clone(),cell.source.state==EvalDetachedCellState::Queued,cell.source.detached,cell.on_kill.clone())};
        if queued {
            let removed=if let Some(kernel)=kernel {kernel.cancel_queued(cell_id,reason).await?} else {false};
            cell.lock().expect("managed cell lock").source.state_retained=Some(true);
            manager.lock().expect("cell manager lock").cancel_without_interrupt(&cell);
            if !removed && let Some(on_kill)=on_kill {on_kill(reason.into());}
        } else if detached {
            let (settled,signal)=tokio::sync::watch::channel(false);
            cell.lock().expect("managed cell lock").interrupt_outcome=Some(signal);
            let cancelled=manager.lock().expect("cell manager lock").cancel_without_interrupt(&cell);
            let outcome=if cancelled && let Some(kernel)=kernel {
                match kernel.interrupt(reason,Some(cell_id)).await {
                    Ok(handle)=>{
                        cell.lock().expect("managed cell lock").source.interrupt_note=handle.note;
                        handle.state_retained.await.map(|retained|cell.lock().expect("managed cell lock").source.state_retained=Some(retained))
                    }
                    Err(error)=>Err(error),
                }
            } else {Ok(())};
            settled.send_replace(true);
            let now=(manager.lock().expect("cell manager lock").options.now)();
            let snapshot=snapshot_detached_cell(&cell.lock().expect("managed cell lock").source,now);
            manager.lock().expect("cell manager lock").terminal_snapshots.remember(snapshot);
            outcome?;
        }
        manager.lock().expect("cell manager lock").peek(cell_id)
    }

    pub fn live_cells(&self, language: Option<EvalLanguage>, except: Option<&str>) -> Vec<EvalDetachedCellSnapshot> {
        let mut live: Vec<_> = self.cells.iter().filter_map(|(_,cell)| {
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
        if let Some((_,cell)) = self.cells.iter().find(|(id,_)|id==cell_id) { return Ok(cell.lock().expect("managed cell poisoned").terminal.subscribe()); }
        let (_, receiver) = tokio::sync::watch::channel(Some(self.peek(cell_id)?));
        Ok(receiver)
    }

    pub async fn flush_notifications(&self) -> Result<(), String> { self.notification_queue.flush().await }

    pub async fn dispose(manager: &Arc<Mutex<Self>>) -> Result<(),String> {
        let cells={
            let locked=manager.lock().expect("cell manager lock");
            locked.detached.iter().map(|(_,cell)| {
                let cell=cell.lock().expect("managed cell lock");
                (cell.source.cell_id.clone(),cell.source.state==EvalDetachedCellState::Queued)
            }).collect::<Vec<_>>()
        };
        let reason="Session ended; detached eval cell cancelled";
        for (id,queued) in &cells {
            if *queued {let _=Self::stop(manager,id,reason).await;}
        }
        let mut stops=cells.iter().map(|(id,_)|Box::pin(Self::stop(manager,id,reason))).collect::<Vec<_>>();
        let mut settled=vec![false;stops.len()];
        std::future::poll_fn(|context| {
            for (stop,settled) in stops.iter_mut().zip(&mut settled) {
                if !*settled && stop.as_mut().poll(context).is_ready() {*settled=true;}
            }
            if settled.iter().all(|settled|*settled) {std::task::Poll::Ready(())} else {std::task::Poll::Pending}
        }).await;
        let flush={
            let locked=manager.lock().expect("cell manager lock");
            if cells.is_empty() {locked.publish_wake_source_state();}
            locked.notification_queue.flush_signal()
        };
        if let Some(mut flush)=flush {
            loop {
                if let Some(result)=flush.borrow().clone() {result?;break;}
                flush.changed().await.map_err(|_|"notification queue retired".to_string())?;
            }
        }
        let mut locked=manager.lock().expect("cell manager lock");
        locked.cells.clear();locked.terminal_snapshots.clear();
        Ok(())
    }

    pub fn cancel_without_interrupt(&mut self, cell: &ManagedCellHandle) -> bool {
        let result = current_detached_result(&cell.lock().expect("managed cell poisoned").source);
        self.settle(cell, EvalDetachedCellState::Cancelled, result)
    }

    fn emit_status(&self) {
        let cells: Vec<_> = self.detached.iter().map(|(_,cell)| cell.lock().expect("managed cell poisoned")).collect();
        let sources: Vec<_> = cells.iter().map(|cell| &cell.source).collect();
        let status = detached_status_entries(&sources);
        let wake = detached_wake_source_state(&sources);
        drop(cells);
        if let Some(callback) = &self.options.on_status_change { callback(status); }
        if let Some(callback) = &self.options.on_wake_source_state { callback(wake); }
    }

    pub fn publish_wake_source_state(&self) {
        let cells: Vec<_> = self.detached.iter().map(|(_,cell)| cell.lock().expect("managed cell poisoned")).collect();
        let sources: Vec<_> = cells.iter().map(|cell| &cell.source).collect();
        let wake = detached_wake_source_state(&sources);
        drop(cells);
        if let Some(callback) = &self.options.on_wake_source_state { callback(wake); }
    }
}
