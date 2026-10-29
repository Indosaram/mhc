use crate::{
    runtime_context::TaskRuntimeContext,
    status_row_format::{is_terminal, task_footer_status},
};
use maho_ext_api::{ExtensionMode, ExtensionUi, ExtensionWidgetOptions};
use senpi_task::{
    manager::{
        TaskManager,
        child_handle::{ManagedChildEvent, ManagedChildListener, Unsubscribe},
        types::ListScope,
    },
    progress::{assistant_last_line, format_tool_activity},
    renderer_text::excerpt_renderer_text,
    state::{TaskRecord, TaskRunStats},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, PoisonError},
};

pub const UI_KEY: &str = "omo-task";
pub trait StatusUiTimers: Send + Sync {
    fn set(&self, callback: Box<dyn FnOnce() + Send>, milliseconds: u64) -> u64;
    fn clear(&self, handle: u64);
}
pub trait StatusUiManager: Send + Sync {
    fn list(&self, session: &str) -> Vec<TaskRecord>;
    fn list_all(&self) -> Vec<TaskRecord> { Vec::new() }
    fn live_run_stats(&self, _task: &str) -> Option<crate::subagent_cost_ledger::LiveRunStats> { None }
    fn has_background_surface(&self) -> bool {
        false
    }
    fn was_background(&self, _task: &str) -> bool {
        false
    }
    fn subscribe_child(&self, _task: &str, _listener: ManagedChildListener) -> Option<Unsubscribe> {
        None
    }
    fn run_stats_snapshot(&self, _task: &str) -> Option<TaskRunStats> {
        None
    }
}
impl StatusUiManager for TaskManager {
    fn list_all(&self) -> Vec<TaskRecord> { self.list(&ListScope::All).into_iter().map(|entry| entry.record).collect() }
    fn live_run_stats(&self, task: &str) -> Option<crate::subagent_cost_ledger::LiveRunStats> {
        let (run_start_epoch, stats) = self.live_run_stats(task)?;
        Some(crate::subagent_cost_ledger::LiveRunStats { run_start_epoch, stats })
    }
    fn list(&self, session: &str) -> Vec<TaskRecord> {
        self.list(&ListScope::ParentSession(session.into()))
            .into_iter()
            .map(|entry| entry.record)
            .collect()
    }
    fn has_background_surface(&self) -> bool {
        true
    }
    fn was_background(&self, task: &str) -> bool {
        self.was_background(task)
    }
    fn subscribe_child(&self, task: &str, listener: ManagedChildListener) -> Option<Unsubscribe> {
        Some(self.subscribe_child(task, listener))
    }
    fn run_stats_snapshot(&self, task: &str) -> Option<TaskRunStats> {
        self.run_stats_snapshot(task)
    }
}
#[derive(Default)]
struct State {
    records: Vec<TaskRecord>,
    activity: BTreeMap<String, String>,
    subscriptions: BTreeMap<String, Unsubscribe>,
    pending: Option<u64>,
    live_refresh: Option<u64>,
    /// A child event arrived since the last record refresh; the next render refreshes first.
    records_stale: bool,
}
pub struct TaskStatusUi {
    pub manager: Arc<dyn StatusUiManager>,
    pub runtime: Arc<Mutex<TaskRuntimeContext>>,
    pub timers: Arc<dyn StatusUiTimers>,
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub terminal_width: Arc<dyn Fn() -> Option<usize> + Send + Sync>,
    cost_feed: Arc<crate::subagent_cost_feed::TreeCostFeed>,
    state: Mutex<State>,
}
impl TaskStatusUi {
    pub fn new(
        manager: Arc<dyn StatusUiManager>,
        runtime: Arc<Mutex<TaskRuntimeContext>>,
        timers: Arc<dyn StatusUiTimers>,
        now: Arc<dyn Fn() -> i64 + Send + Sync>,
        terminal_width: Arc<dyn Fn() -> Option<usize> + Send + Sync>,
    ) -> Arc<Self> {
        let cost_feed = crate::subagent_cost_feed::TreeCostFeed::new(manager.clone(), runtime.clone());
        Arc::new(Self {
            cost_feed,
            manager,
            runtime,
            timers,
            now,
            terminal_width,
            state: Mutex::new(State::default()),
        })
    }
    fn ui(&self) -> Option<Arc<dyn ExtensionUi>> {
        let runtime = self.runtime.lock().unwrap_or_else(PoisonError::into_inner);
        if runtime
            .mode()
            .is_some_and(|mode| mode != ExtensionMode::Tui)
        {
            None
        } else {
            runtime.ui().cloned()
        }
    }
    fn refresh_cached_records(self: &Arc<Self>) -> bool {
        if self.ui().is_none() {
            return false;
        }
        // Clear before listing so a child event racing `list` re-marks the cache stale.
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .records_stale = false;
        let session = self
            .runtime
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .session_id()
            .map(str::to_owned);
        let mut records = session.map_or_else(Vec::new, |session| self.manager.list(&session));
        for record in &mut records {
            record.final_response = None;
            record.error_message = None;
            record.description = None;
            record.spawn_spec = None;
        }
        let active: Vec<_> = records
            .iter()
            .filter(|record| {
                !is_terminal(record.status) && self.manager.was_background(&record.task_id)
            })
            .map(|record| record.task_id.clone())
            .collect();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.records = records;
        let active_ids: std::collections::HashSet<&str> =
            active.iter().map(String::as_str).collect();
        let stale: Vec<_> = state
            .subscriptions
            .keys()
            .filter(|id| !active_ids.contains(id.as_str()))
            .cloned()
            .collect();
        for id in stale {
            if let Some(unsubscribe) = state.subscriptions.remove(&id) {
                unsubscribe();
            }
            state.activity.remove(&id);
        }
        for id in active {
            if state.subscriptions.contains_key(&id) {
                continue;
            }
            let weak = Arc::downgrade(self);
            let task_id = id.clone();
            let listener = Arc::new(move |event: &ManagedChildEvent| {
                if let Some(ui) = weak.upgrade()
                    && let Some(activity) = activity_from_event(event)
                {
                    ui.state
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .activity
                        .insert(task_id.clone(), activity);
                    ui.schedule_activity_sync();
                }
            });
            if let Some(unsubscribe) = self.manager.subscribe_child(&id, listener) {
                state.subscriptions.insert(id, unsubscribe);
            }
        }
        true
    }
    pub fn observe_record(self: &Arc<Self>, change: &crate::store_mutation_observer::StoreMutation) { self.cost_feed.observe(change); }
    pub fn sync_now(self: &Arc<Self>) {
        self.cost_feed.ensure_seeded();
        if !self.refresh_cached_records() {
            self.clear_live_refresh();
            return;
        }
        self.render_cached_records();
    }
    fn render_cached_records(self: &Arc<Self>) {
        let Some(ui) = self.ui() else {
            self.clear_live_refresh();
            return;
        };
        let records = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .records
            .clone();
        // DAG node tasks are spawned in the background but already count in the omo-dag status.
        let background: Vec<_> = records
            .iter()
            .filter(|record| record.owner.is_none() && self.manager.was_background(&record.task_id))
            .cloned()
            .collect();
        let shown = if self.manager.has_background_surface() {
            &background
        } else {
            &records
        };
        ui.set_widget(UI_KEY, None, ExtensionWidgetOptions::default());
        let Some(status) = task_footer_status(shown) else {
            self.clear_live_refresh();
            ui.set_status(UI_KEY, None);
            return;
        };
        ui.set_status(UI_KEY, Some(&status));
        if self.manager.has_background_surface()
            && background.iter().any(|record| !is_terminal(record.status))
        {
            self.schedule_live_refresh();
        } else {
            self.clear_live_refresh();
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.subscriptions.is_empty() {
                state.records = Vec::new();
            }
        }
    }
    pub fn schedule_sync(self: &Arc<Self>) {
        let refreshed = self.refresh_cached_records();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.live_refresh.is_some() {
            return;
        }
        if let Some(handle) = state.pending.take() {
            self.timers.clear(handle);
        }
        let weak = Arc::downgrade(self);
        state.pending = Some(self.timers.set(
            Box::new(move || {
                if let Some(ui) = weak.upgrade() {
                    ui.state
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .pending = None;
                    if refreshed {
                        ui.render_cached_records();
                    } else {
                        ui.sync_now();
                    }
                }
            }),
            250,
        ));
    }
    /// Child-activity variant of [`Self::schedule_sync`]: defers the record refresh to render time
    /// instead of re-listing every record on each child event.
    fn schedule_activity_sync(self: &Arc<Self>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.records_stale = true;
        if state.live_refresh.is_some() {
            return;
        }
        if let Some(handle) = state.pending.take() {
            self.timers.clear(handle);
        }
        let weak = Arc::downgrade(self);
        state.pending = Some(self.timers.set(
            Box::new(move || {
                if let Some(ui) = weak.upgrade() {
                    ui.state
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .pending = None;
                    ui.sync_now();
                }
            }),
            250,
        ));
    }
    fn schedule_live_refresh(self: &Arc<Self>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.live_refresh.is_some() {
            return;
        }
        let weak = Arc::downgrade(self);
        state.live_refresh = Some(self.timers.set(
            Box::new(move || {
                if let Some(ui) = weak.upgrade() {
                    let stale = {
                        let mut state = ui.state.lock().unwrap_or_else(PoisonError::into_inner);
                        state.live_refresh = None;
                        std::mem::take(&mut state.records_stale)
                    };
                    if stale {
                        ui.sync_now();
                    } else {
                        ui.render_cached_records();
                    }
                }
            }),
            250,
        ));
    }
    fn clear_live_refresh(&self) {
        if let Some(handle) = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .live_refresh
            .take()
        {
            self.timers.clear(handle);
        }
    }
    pub fn dispose(&self) {
        self.cost_feed.dispose();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(handle) = state.pending.take() {
            self.timers.clear(handle);
        }
        if let Some(handle) = state.live_refresh.take() {
            self.timers.clear(handle);
        }
        for (_, unsubscribe) in std::mem::take(&mut state.subscriptions) {
            unsubscribe();
        }
        state.activity.clear();
        state.records.clear();
    }
}
pub fn activity_from_event(event: &ManagedChildEvent) -> Option<String> {
    match event.event_type.as_str() {
        "tool_execution_start" => event.tool_name.as_deref().map(|tool| {
            excerpt_renderer_text(
                &format_tool_activity(tool, event.args.as_ref().or(event.input.as_ref())),
                Some(32),
            )
        }),
        "tool_execution_end" => Some("running".into()),
        "message_end" => assistant_last_line(event.message.as_ref())
            .map(|text| excerpt_renderer_text(&text, Some(32))),
        _ => None,
    }
}
