use std::{collections::BTreeMap, sync::{Arc, Mutex, PoisonError}};
use maho_ext_api::{ExtensionMode, ExtensionUi, ExtensionWidgetOptions, WidgetContent, WidgetPlacement};
use senpi_task::{manager::{TaskManager, child_handle::{ManagedChildEvent, ManagedChildListener, Unsubscribe}, types::ListScope}, progress::{assistant_last_line, format_tool_activity}, renderer_text::excerpt_renderer_text, state::{TaskRecord, TaskRunStats}};
use crate::{runtime_context::TaskRuntimeContext, status_row_format::{background_widget_rows, build_widget_rows, is_terminal}};

pub const UI_KEY: &str = "omo-task";
pub trait StatusUiTimers: Send + Sync {
    fn set(&self, callback: Box<dyn FnOnce() + Send>, milliseconds: u64) -> u64;
    fn clear(&self, handle: u64);
}
pub trait StatusUiManager: Send + Sync {
    fn list(&self, session: &str) -> Vec<TaskRecord>;
    fn has_background_surface(&self) -> bool { false }
    fn was_background(&self, _task: &str) -> bool { false }
    fn subscribe_child(&self, _task: &str, _listener: ManagedChildListener) -> Option<Unsubscribe> { None }
    fn run_stats_snapshot(&self, _task: &str) -> Option<TaskRunStats> { None }
}
impl StatusUiManager for TaskManager {
    fn list(&self, session: &str) -> Vec<TaskRecord> { self.list(&ListScope::ParentSession(session.into())).into_iter().map(|entry| entry.record).collect() }
    fn has_background_surface(&self) -> bool { true }
    fn was_background(&self, task: &str) -> bool { self.was_background(task) }
    fn subscribe_child(&self, task: &str, listener: ManagedChildListener) -> Option<Unsubscribe> { Some(self.subscribe_child(task, listener)) }
    fn run_stats_snapshot(&self, task: &str) -> Option<TaskRunStats> { self.run_stats_snapshot(task) }
}
#[derive(Default)]
struct State {
    records: Vec<TaskRecord>,
    activity: BTreeMap<String, String>,
    subscriptions: BTreeMap<String, Unsubscribe>,
    pending: Option<u64>,
    live_refresh: Option<u64>,
}
pub struct TaskStatusUi {
    pub manager: Arc<dyn StatusUiManager>,
    pub runtime: Arc<Mutex<TaskRuntimeContext>>,
    pub timers: Arc<dyn StatusUiTimers>,
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub terminal_width: Arc<dyn Fn() -> Option<usize> + Send + Sync>,
    state: Mutex<State>,
}
impl TaskStatusUi {
    pub fn new(manager: Arc<dyn StatusUiManager>, runtime: Arc<Mutex<TaskRuntimeContext>>, timers: Arc<dyn StatusUiTimers>, now: Arc<dyn Fn() -> i64 + Send + Sync>, terminal_width: Arc<dyn Fn() -> Option<usize> + Send + Sync>) -> Arc<Self> {
        Arc::new(Self { manager, runtime, timers, now, terminal_width, state: Mutex::new(State::default()) })
    }
    fn ui(&self) -> Option<Arc<dyn ExtensionUi>> {
        let runtime = self.runtime.lock().unwrap_or_else(PoisonError::into_inner);
        if runtime.mode().is_some_and(|mode| mode != ExtensionMode::Tui) { None } else { runtime.ui().cloned() }
    }
    fn refresh_cached_records(self: &Arc<Self>) -> bool {
        if self.ui().is_none() { return false }
        let session = self.runtime.lock().unwrap_or_else(PoisonError::into_inner).session_id().map(str::to_owned);
        let records = session.map_or_else(Vec::new, |session| self.manager.list(&session));
        let active: Vec<_> = records.iter().filter(|record| !is_terminal(record.status) && self.manager.was_background(&record.task_id)).map(|record| record.task_id.clone()).collect();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.records = records;
        let stale: Vec<_> = state.subscriptions.keys().filter(|id| !active.contains(id)).cloned().collect();
        for id in stale { if let Some(unsubscribe) = state.subscriptions.remove(&id) { unsubscribe(); } state.activity.remove(&id); }
        for id in active {
            if state.subscriptions.contains_key(&id) { continue }
            let weak = Arc::downgrade(self);
            let task_id = id.clone();
            let listener = Arc::new(move |event: &ManagedChildEvent| {
                if let Some(ui) = weak.upgrade() && let Some(activity) = activity_from_event(event) {
                    ui.state.lock().unwrap_or_else(PoisonError::into_inner).activity.insert(task_id.clone(), activity);
                    ui.schedule_sync();
                }
            });
            if let Some(unsubscribe) = self.manager.subscribe_child(&id, listener) { state.subscriptions.insert(id, unsubscribe); }
        }
        true
    }
    pub fn sync_now(self: &Arc<Self>) {
        if !self.refresh_cached_records() { self.clear_live_refresh(); return }
        self.render_cached_records();
    }
    fn render_cached_records(self: &Arc<Self>) {
        let Some(ui) = self.ui() else { self.clear_live_refresh(); return };
        let (records, activity) = { let state = self.state.lock().unwrap_or_else(PoisonError::into_inner); (state.records.clone(), state.activity.clone()) };
        let background: Vec<_> = records.iter().filter(|record| self.manager.was_background(&record.task_id)).cloned().collect();
        let rows = if self.manager.has_background_surface() { background_widget_rows(&background, &activity, (self.now)(), &|id| self.manager.run_stats_snapshot(id), (self.terminal_width)()) } else { build_widget_rows(&records) };
        if rows.is_empty() { self.clear_live_refresh(); ui.set_widget(UI_KEY, None, ExtensionWidgetOptions::default()); return }
        ui.set_widget(UI_KEY, Some(WidgetContent::Lines(rows)), ExtensionWidgetOptions { placement: WidgetPlacement::BelowEditor });
        if self.manager.has_background_surface() && background.iter().any(|record| !is_terminal(record.status)) { self.schedule_live_refresh(); } else { self.clear_live_refresh(); }
    }
    pub fn schedule_sync(self: &Arc<Self>) {
        let refreshed = self.refresh_cached_records();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.live_refresh.is_some() { return }
        if let Some(handle) = state.pending.take() { self.timers.clear(handle); }
        let weak = Arc::downgrade(self);
        state.pending = Some(self.timers.set(Box::new(move || { if let Some(ui) = weak.upgrade() { ui.state.lock().unwrap_or_else(PoisonError::into_inner).pending = None; if refreshed { ui.render_cached_records(); } else { ui.sync_now(); } } }), 250));
    }
    fn schedule_live_refresh(self: &Arc<Self>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.live_refresh.is_some() { return }
        let weak = Arc::downgrade(self);
        state.live_refresh = Some(self.timers.set(Box::new(move || { if let Some(ui) = weak.upgrade() { ui.state.lock().unwrap_or_else(PoisonError::into_inner).live_refresh = None; ui.render_cached_records(); } }), 250));
    }
    fn clear_live_refresh(&self) { if let Some(handle) = self.state.lock().unwrap_or_else(PoisonError::into_inner).live_refresh.take() { self.timers.clear(handle); } }
    pub fn dispose(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(handle) = state.pending.take() { self.timers.clear(handle); }
        if let Some(handle) = state.live_refresh.take() { self.timers.clear(handle); }
        for (_, unsubscribe) in std::mem::take(&mut state.subscriptions) { unsubscribe(); }
        state.activity.clear(); state.records.clear();
    }
}
pub fn activity_from_event(event: &ManagedChildEvent) -> Option<String> {
    match event.event_type.as_str() {
        "tool_execution_start" => event.tool_name.as_deref().map(|tool| excerpt_renderer_text(&format_tool_activity(tool, event.args.as_ref().or(event.input.as_ref())), Some(32))),
        "tool_execution_end" => Some("running".into()),
        "message_end" => assistant_last_line(event.message.as_ref()).map(|text| excerpt_renderer_text(&text, Some(32))),
        _ => None,
    }
}
