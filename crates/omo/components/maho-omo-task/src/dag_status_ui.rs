use crate::{
    dag_status_row_format::dag_footer_status, runtime_context::TaskRuntimeContext,
    status_ui::StatusUiTimers,
};
use maho_ext_api::{ExtensionMode, ExtensionWidgetOptions};
use senpi_task::dag::{manager::DagRunSummary, types::DagRunSnapshot};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, PoisonError},
};

pub const DAG_STATUS_UI_KEY: &str = "omo-dag";
type StatusLogger = Arc<dyn Fn(&str, &str) + Send + Sync>;
pub trait DagStatusUiManager: Send + Sync {
    fn list(&self, session: &str) -> Vec<DagRunSummary>;
    fn snapshot(&self, run: &str, session: &str) -> Option<DagRunSnapshot>;
}
#[derive(Default)]
struct State {
    activity: BTreeMap<String, BTreeMap<String, String>>,
    pending: Option<u64>,
    live_refresh: Option<u64>,
    faults: BTreeMap<String, ()>,
}
pub struct DagStatusUi {
    manager: Arc<dyn DagStatusUiManager>,
    runtime: Arc<Mutex<TaskRuntimeContext>>,
    timers: Arc<dyn StatusUiTimers>,
    logger: Option<StatusLogger>,
    state: Mutex<State>,
}
impl DagStatusUi {
    pub fn new(
        manager: Arc<dyn DagStatusUiManager>,
        runtime: Arc<Mutex<TaskRuntimeContext>>,
        timers: Arc<dyn StatusUiTimers>,
    ) -> Arc<Self> {
        Self::with_options(manager, runtime, timers, None)
    }
    pub fn with_options(
        manager: Arc<dyn DagStatusUiManager>,
        runtime: Arc<Mutex<TaskRuntimeContext>>,
        timers: Arc<dyn StatusUiTimers>,
        logger: Option<StatusLogger>,
    ) -> Arc<Self> {
        Arc::new(Self {
            manager,
            runtime,
            timers,
            logger,
            state: Mutex::new(State::default()),
        })
    }
    pub fn sync_now(self: &Arc<Self>) {
        if let Err(payload) =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.render_now()))
        {
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
                .unwrap_or_else(|| "render fault".into());
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.faults.insert(message.clone(), ()).is_none() {
                if let Some(logger) = &self.logger {
                    logger(
                        "omo-dag status widget render failed; frame skipped",
                        &message,
                    );
                } else {
                    maho_ext_api::diagnostic(&format!(
                        "omo-dag status widget render failed; frame skipped: {message}"
                    ));
                }
            }
        }
    }
    fn render_now(self: &Arc<Self>) {
        let (ui, session) = {
            let runtime = self.runtime.lock().unwrap_or_else(PoisonError::into_inner);
            if runtime.mode() != Some(ExtensionMode::Tui) {
                drop(runtime);
                self.clear_live_refresh();
                return;
            }
            (
                runtime.ui().cloned(),
                runtime.session_id().map(str::to_owned),
            )
        };
        let Some(ui) = ui else {
            self.clear_live_refresh();
            return;
        };
        let runs: Vec<_> = if let Some(session) = session.as_deref() {
            self.manager
                .list(session)
                .into_iter()
                .filter(|run| !run.status.is_terminal())
                .filter_map(|run| {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        self.manager.snapshot(&run.run_id, session)
                    }))
                    .ok()
                    .flatten()
                })
                .collect()
        } else {
            Vec::new()
        };
        let status = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state
                .activity
                .retain(|id, _| runs.iter().any(|run| &run.run_id == id));
            dag_footer_status(&runs)
        };
        let live_count = if status.is_none() {
            session.as_deref().map_or(0, |session| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.manager.list(session)
                }))
                .unwrap_or_default()
                .iter()
                .filter(|run| !run.status.is_terminal())
                .count()
            })
        } else {
            0
        };
        if status.is_none() && live_count > 0 {
            ui.set_status(
                DAG_STATUS_UI_KEY,
                Some(&format!(
                    "dag tracking {live_count} run{}",
                    if live_count == 1 { "" } else { "s" }
                )),
            );
            ui.set_widget(DAG_STATUS_UI_KEY, None, ExtensionWidgetOptions::default());
            self.schedule_live_refresh();
            return;
        }
        let Some(status) = status else {
            self.clear_live_refresh();
            ui.set_widget(DAG_STATUS_UI_KEY, None, ExtensionWidgetOptions::default());
            ui.set_status(DAG_STATUS_UI_KEY, None);
            return;
        };
        ui.set_status(DAG_STATUS_UI_KEY, Some(&status));
        ui.set_widget(DAG_STATUS_UI_KEY, None, ExtensionWidgetOptions::default());
        if runs.iter().any(|run| !run.status.is_terminal()) {
            self.schedule_live_refresh();
        } else {
            self.clear_live_refresh();
        }
    }
    pub fn on_activity(self: &Arc<Self>, run: &str, node: &str, activity: &str) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .activity
            .entry(run.into())
            .or_default()
            .insert(node.into(), activity.into());
        self.schedule_sync();
    }
    pub fn schedule_sync(self: &Arc<Self>) {
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
                    ui.state
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .live_refresh = None;
                    ui.sync_now();
                }
            }),
            1000,
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
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(handle) = state.pending.take() {
            self.timers.clear(handle);
        }
        if let Some(handle) = state.live_refresh.take() {
            self.timers.clear(handle);
        }
        state.activity.clear();
    }
}
