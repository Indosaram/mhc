//! Session-owned live collector; replacing or retiring an owner drops its callbacks.
use crate::{turn_end_liveness::TurnEndLivenessInput, types::Goal};
use std::sync::{Arc, Mutex, Weak};
use std::{collections::BTreeMap, path::PathBuf};

fn services() -> &'static Mutex<BTreeMap<PathBuf, Weak<GoalLifecycleCollector>>> {
    static SERVICES: std::sync::OnceLock<Mutex<BTreeMap<PathBuf, Weak<GoalLifecycleCollector>>>> =
        std::sync::OnceLock::new();
    SERVICES.get_or_init(Default::default)
}
/// Discover the same live service used by the goal builtin, without owning its lifetime.
pub fn session_lifecycle(
    reference: &crate::types::GoalStoreRef,
) -> Option<Arc<GoalLifecycleCollector>> {
    let mut services = services()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    services.retain(|_, service| service.strong_count() > 0);
    services
        .get(&crate::store::goal_file_path(reference))
        .and_then(Weak::upgrade)
}
pub fn bind_session_lifecycle(
    reference: &crate::types::GoalStoreRef,
    service: &Arc<GoalLifecycleCollector>,
) {
    services()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            crate::store::goal_file_path(reference),
            Arc::downgrade(service),
        );
}
pub fn notify_transition(reference: &crate::types::GoalStoreRef, goal: Option<&Goal>) {
    if let Some(service) = session_lifecycle(reference) {
        service.goal_lifecycle_changed(goal);
    }
}
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
    #[tokio::test]
    async fn store_transitions_are_observed_before_the_next_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let reference = crate::types::GoalStoreRef {
            base_dir: dir.path().into(),
            thread_id: "live".into(),
        };
        let service = Arc::new(GoalLifecycleCollector::default());
        bind_session_lifecycle(&reference, &service);
        assert!(Arc::ptr_eq(
            &session_lifecycle(&reference).unwrap(),
            &service
        ));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let capture = seen.clone();
        service.on_goal_lifecycle_change(Arc::new(move |goal| {
            capture.lock().unwrap().push(goal.map(|goal| goal.status));
        }));
        crate::store::create_goal(&reference, "work", None, 1)
            .await
            .unwrap();
        crate::store::update_goal(
            &reference,
            &crate::types::GoalUpdate {
                status: Some(crate::types::GoalStatus::Complete),
                ..Default::default()
            },
            crate::types::GoalUpdateSource::Model,
            2,
        )
        .await
        .unwrap();
        crate::store::clear_goal(&reference).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                Some(crate::types::GoalStatus::Active),
                Some(crate::types::GoalStatus::Complete),
                None
            ]
        );
        drop(service);
        assert!(session_lifecycle(&reference).is_none());
    }
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
