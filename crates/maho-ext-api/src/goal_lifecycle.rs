//! Neutral goal facts shared by producers and the core lifecycle reducer.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, OnceLock, Weak},
};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LifecycleGoal {
    pub id: String,
    pub status: String,
    pub continuation_stopped: bool,
}
#[derive(Clone, Debug, Default)]
pub struct GoalLifecycleSnapshot {
    pub goal: Option<LifecycleGoal>,
    pub input: Option<serde_json::Value>,
    /// Captured synchronously, so complete -> clear between snapshots is not lost.
    pub completed_goal_id: Option<String>,
}
#[derive(Default)]
pub struct GoalLifecycleService(Mutex<GoalLifecycleSnapshot>);
impl GoalLifecycleService {
    pub fn snapshot(&self) -> GoalLifecycleSnapshot {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    pub fn transition(&self, goal: Option<LifecycleGoal>) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(goal) = &goal
            && goal.status == "complete"
                && state
                    .goal
                    .as_ref()
                    .is_none_or(|old| old.id != goal.id || old.status != "complete")
            {
                state.completed_goal_id = Some(goal.id.clone());
            }
        state.goal = goal;
    }
    pub fn turn_end(&self, input: serde_json::Value) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .input = Some(input);
    }
    pub fn start_run(&self) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .input = None;
    }
}
pub fn goal_lifecycle_service(session_id: &str) -> Arc<GoalLifecycleService> {
    static SERVICES: OnceLock<Mutex<BTreeMap<String, Weak<GoalLifecycleService>>>> =
        OnceLock::new();
    let mut services = SERVICES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    services.retain(|_, service| service.strong_count() > 0);
    if let Some(service) = services.get(session_id).and_then(Weak::upgrade) {
        return service;
    }
    let service = Arc::new(GoalLifecycleService::default());
    services.insert(session_id.into(), Arc::downgrade(&service));
    service
}
