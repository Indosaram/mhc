use crate::dag_status_ui::DagStatusUiManager;
use maho_ext_api::{BusSubscription, EventBus};
use serde_json::{Value, json};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex, PoisonError, RwLock},
};

pub const DAG_WAKE_SOURCE_STATE_EVENT: &str = "wake_source_state";
pub const DAG_WAKE_SOURCE_STATE_REQUEST_EVENT: &str = "wake_source_state_request";
pub const DAG_WAKE_SOURCE: &str = "omo-dag";

pub struct DagWakeSourceDeps {
    pub events: EventBus,
    pub manager: Arc<dyn DagStatusUiManager>,
    pub session_id: Arc<dyn Fn() -> Option<String> + Send + Sync>,
}

struct DagWakeSourceInner {
    manager: Arc<dyn DagStatusUiManager>,
    session_id: Arc<dyn Fn() -> Option<String> + Send + Sync>,
}

pub struct DagWakeSource {
    pub events: EventBus,
    pub manager: Arc<dyn DagStatusUiManager>,
    pub session_id: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    inner: Arc<RwLock<DagWakeSourceInner>>,
    subscription: Mutex<Option<BusSubscription>>,
}

pub fn create_dag_wake_source(deps: DagWakeSourceDeps) -> DagWakeSource {
    let inner = Arc::new(RwLock::new(DagWakeSourceInner {
        manager: deps.manager.clone(),
        session_id: deps.session_id.clone(),
    }));
    let weak_inner = Arc::downgrade(&inner);
    let events = deps.events.clone();
    let subscription = deps.events.on(
        DAG_WAKE_SOURCE_STATE_REQUEST_EVENT,
        Arc::new(move |_| {
            let Some(inner) = weak_inner.upgrade() else { return };
            let (manager, session_id) = {
                let guard = inner.read().unwrap_or_else(PoisonError::into_inner);
                (guard.manager.clone(), guard.session_id.clone())
            };
            let channels = collect_channels(manager.as_ref(), &*session_id);
            events.emit(DAG_WAKE_SOURCE_STATE_EVENT, &json!({"source":DAG_WAKE_SOURCE,"kind":"children","activeCount":channels.len(),"channels":channels}));
        }),
    );
    DagWakeSource {
        events: deps.events,
        manager: deps.manager,
        session_id: deps.session_id,
        inner,
        subscription: Mutex::new(Some(subscription)),
    }
}

fn collect_channels(
    manager: &dyn DagStatusUiManager,
    session_id: &(dyn Fn() -> Option<String> + Send + Sync),
) -> Vec<Value> {
    (session_id)().map_or_else(Vec::new, |session| {
        manager
            .list(&session)
            .into_iter()
            .filter(|run| !run.status.is_terminal())
            .filter_map(|run| {
                catch_unwind(AssertUnwindSafe(|| manager.snapshot(&run.run_id, &session)))
                    .ok()
                    .flatten()
            })
            .map(|run| {
                let date = run.started_at.as_deref().unwrap_or(&run.created_at);
                let started = chrono::DateTime::parse_from_rfc3339(date)
                    .map_or(Value::Null, |date| json!(date.timestamp_millis()));
                json!({"id":run.run_id,"description":run.name,"startedAtMs":started})
            })
            .collect()
    })
}

impl DagWakeSource {
    pub fn on_run_start(&self, _run_id: &str) {
        self.publish_live();
    }
    pub fn on_run_terminal(&self, _run_id: &str) {
        self.publish_live();
    }
    pub fn publish_live(&self) {
        {
            let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
            inner.manager = self.manager.clone();
            inner.session_id = self.session_id.clone();
        }
        let channels = collect_channels(self.manager.as_ref(), &*self.session_id);
        self.publish(channels);
    }
    pub fn emit_shutdown(&self) {
        drop(
            self.subscription
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        self.publish(vec![]);
    }
    /// senpi#3864: a live workflow run is children's work, not a background job, so the published
    /// state carries `kind: "children"` and continuation keeps the parent's lifecycle off "done".
    fn publish(&self, channels: Vec<Value>) {
        self.events.emit(DAG_WAKE_SOURCE_STATE_EVENT, &json!({"source":DAG_WAKE_SOURCE,"kind":"children","activeCount":channels.len(),"channels":channels}));
    }
}
