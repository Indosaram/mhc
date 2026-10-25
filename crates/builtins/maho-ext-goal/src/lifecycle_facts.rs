//! Session-owned live collector; replacing or retiring an owner drops its callbacks.
use crate::{turn_end_liveness::TurnEndLivenessInput, types::Goal};
use std::sync::{Arc, Mutex};
#[derive(Clone)]
pub struct GoalLifecycleFacts {
    pub goal: Option<Goal>,
    pub input: Option<TurnEndLivenessInput>,
}
pub type Collector = Arc<dyn Fn() -> GoalLifecycleFacts + Send + Sync>;
pub type Observer = Arc<dyn Fn(Option<&Goal>) + Send + Sync>;
#[derive(Default)]
pub struct GoalLifecycleCollector {
    collector: Mutex<Option<Collector>>,
    observer: Mutex<Option<Observer>>,
}
impl GoalLifecycleCollector {
    pub fn on_goal_lifecycle_change(&self, observer: Observer) {
        *self
            .observer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(observer);
    }
    pub fn goal_lifecycle_changed(&self, goal: Option<&Goal>) {
        let observer = self
            .observer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(observer) = observer {
            observer(goal);
        }
    }
    pub fn register_goal_lifecycle_collector(&self, collect: Collector) {
        *self
            .collector
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(collect);
    }
    pub fn collect_goal_lifecycle_facts(&self) -> Option<GoalLifecycleFacts> {
        let collector = self
            .collector
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        collector.map(|collect| collect())
    }
    pub fn retire(&self) {
        self.collector
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.observer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replacement_reads_live_facts_and_retirement_removes_owners() {
        let owner = GoalLifecycleCollector::default();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let capture = count.clone();
        owner.on_goal_lifecycle_change(Arc::new(move |_| {
            capture.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        owner.register_goal_lifecycle_collector(Arc::new(|| GoalLifecycleFacts {
            goal: None,
            input: None,
        }));
        assert!(owner.collect_goal_lifecycle_facts().is_some());
        owner.goal_lifecycle_changed(None);
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
        owner.retire();
        assert!(owner.collect_goal_lifecycle_facts().is_none());
        owner.goal_lifecycle_changed(None);
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
