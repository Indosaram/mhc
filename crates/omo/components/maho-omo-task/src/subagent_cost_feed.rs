use std::{collections::{BTreeMap, BTreeSet}, sync::{Arc, Mutex, PoisonError}};
use maho_ext_api::{ExtensionMode, ExtensionUi};
use senpi_task::{manager::child_handle::Unsubscribe, state::TaskRecord};
use crate::{runtime_context::TaskRuntimeContext, status_ui::StatusUiManager, store_mutation_observer::StoreMutation, subagent_cost_ledger::SubagentCostLedger};
#[derive(Default)]
struct State {
    ledger: SubagentCostLedger,
    subscriptions: BTreeMap<String, Unsubscribe>,
    sessions: BTreeSet<String>,
    seeded_for: Option<String>,
    published: Option<(String, Option<f64>)>,
}
pub struct TreeCostFeed {
    manager: Arc<dyn StatusUiManager>,
    runtime: Arc<Mutex<TaskRuntimeContext>>,
    state: Mutex<State>,
}
impl TreeCostFeed {
    pub fn new(manager: Arc<dyn StatusUiManager>, runtime: Arc<Mutex<TaskRuntimeContext>>) -> Arc<Self> {
        Arc::new(Self { manager, runtime, state: Mutex::new(State::default()) })
    }
    fn target(&self) -> Option<(String, Arc<dyn ExtensionUi>)> {
        let runtime = self.runtime.lock().unwrap_or_else(PoisonError::into_inner);
        let ui = runtime.ui()?.clone();
        if !ui.supports_subagent_cost() || runtime.mode().is_some_and(|mode| mode != ExtensionMode::Tui) { return None; }
        Some((runtime.session_id()?.to_owned(), ui))
    }
    fn belongs(&self, record: &TaskRecord) -> bool {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.sessions.contains(&record.parent_session_id) || state.sessions.contains(&record.root_session_id)
    }
    fn publish(&self) {
        let Some((session, ui)) = self.target() else { return; };
        let cost = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let total = state.ledger.total();
            let cost = (total > 0.0).then_some(total);
            if state.published.as_ref() == Some(&(session.clone(), cost)) { return; }
            state.published = Some((session, cost));
            cost
        };
        ui.set_subagent_cost(cost);
    }
    fn track(self: &Arc<Self>, record: &TaskRecord) {
        let (unsettled, subscribed, unsubscribe) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(session) = &record.child_session_id { state.sessions.insert(session.clone()); }
            state.ledger.upsert_record(record);
            let unsettled = state.ledger.is_unsettled(&record.task_id);
            let subscribed = state.subscriptions.contains_key(&record.task_id);
            let unsubscribe = if unsettled { None } else { state.subscriptions.remove(&record.task_id) };
            (unsettled, subscribed, unsubscribe)
        };
        if let Some(unsubscribe) = unsubscribe { unsubscribe(); }
        if !unsettled { return; }
        self.refresh_live(&record.task_id);
        if subscribed { return; }
        let weak = Arc::downgrade(self);
        let task = record.task_id.clone();
        let listener = Arc::new(move |event: &senpi_task::manager::child_handle::ManagedChildEvent| {
            if event.event_type == "message_end" && let Some(feed) = weak.upgrade() { feed.refresh_live(&task); feed.publish(); }
        });
        if let Some(unsubscribe) = self.manager.subscribe_child(&record.task_id, listener) {
            self.state.lock().unwrap_or_else(PoisonError::into_inner).subscriptions.insert(record.task_id.clone(), unsubscribe);
        }
    }
    fn refresh_live(&self, task: &str) {
        let live = self.manager.live_run_stats(task);
        self.state.lock().unwrap_or_else(PoisonError::into_inner).ledger.upsert_live(task, live.as_ref());
    }
    fn seed(self: &Arc<Self>, session: &str) {
        self.dispose();
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.sessions.insert(session.into());
            state.seeded_for = Some(session.into());
        }
        let mut pending = self.manager.list_all();
        loop {
            let mut admitted = false;
            let mut index = pending.len();
            while index > 0 {
                index -= 1;
                if self.belongs(&pending[index]) { let record = pending.remove(index); self.track(&record); admitted = true; }
            }
            if !admitted { break; }
        }
    }
    pub fn ensure_seeded(self: &Arc<Self>) {
        let Some((session, _)) = self.target() else { return; };
        if self.state.lock().unwrap_or_else(PoisonError::into_inner).seeded_for.as_deref() == Some(&session) { return; }
        self.seed(&session);
        self.publish();
    }
    pub fn observe(self: &Arc<Self>, change: &StoreMutation) {
        let Some((session, _)) = self.target() else { return; };
        if self.state.lock().unwrap_or_else(PoisonError::into_inner).seeded_for.as_deref() != Some(&session) { self.seed(&session); }
        if let Some(record) = &change.record {
            if self.belongs(record) {
                let reread = self.state.lock().unwrap_or_else(PoisonError::into_inner).ledger.is_unsettled(&change.task_id) && record.run_stats.is_some() && record.execution_mode != "in-process";
                self.track(record);
                if reread { self.seed(&session); }
            }
        } else {
            let unsubscribe = {
                let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                state.ledger.remove(&change.task_id);
                state.subscriptions.remove(&change.task_id)
            };
            if let Some(unsubscribe) = unsubscribe { unsubscribe(); }
        }
        self.publish();
    }
    pub fn dispose(&self) {
        let subscriptions = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let subscriptions = std::mem::take(&mut state.subscriptions);
            state.ledger.clear(); state.sessions.clear(); state.seeded_for = None; state.published = None;
            subscriptions
        };
        for (_, unsubscribe) in subscriptions { unsubscribe(); }
    }
}
