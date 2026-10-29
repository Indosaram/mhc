//! `engine-store-chain.ts`: the task record store wrapper chain, innermost first.
//!
//! 1. completion-observing: terminal TRANSITIONS drive parent notification and member liveness (F7);
//! 2. mutation-notifying: the debounced UI sync plus the terminal status-edge ledger, which is the
//!    only layer that also sees the `lost` writes reconciliation makes through replace/mutate.
//!
//! The mutation notification carries the record and the status it replaced, so the status spend
//! ledger observes cost at mutation time instead of re-reading the store.

use std::sync::Arc;

use senpi_task::{
    completion::{CompletionNotifier, ParentState},
    state::{TaskRecord, TaskStatus},
    store::TaskRecordStore,
};

use crate::completion_bridge::{CompletionBridgeDeps, create_completion_observing_store};
use crate::store_mutation_observer::{
    MutationListener, StoreMutation, create_mutation_notifying_store,
};

pub struct TaskStoreChainDeps {
    pub base_store: TaskRecordStore,
    pub notifier: Arc<CompletionNotifier>,
    pub parent_state: Arc<dyn Fn() -> ParentState + Send + Sync>,
    pub was_background: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    pub on_terminal: Option<crate::completion_bridge::TerminalObserver>,
}

/// One listener of every record mutation; the returned closure unsubscribes it.
pub type MutationSubscription = Arc<dyn Fn() + Send + Sync>;

pub struct TaskStoreChain {
    pub store: crate::store_mutation_observer::MutationNotifyingStore,
    listeners: Arc<std::sync::Mutex<Vec<MutationListener>>>,
}

impl TaskStoreChain {
    /// Subscribe to every record mutation (spawn/transition/replace/remove).
    pub fn on_mutation(&self, listener: MutationListener) -> MutationSubscription {
        self.listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(listener.clone());
        let listeners = Arc::clone(&self.listeners);
        Arc::new(move || {
            listeners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retain(|entry| !Arc::ptr_eq(entry, &listener));
        })
    }

    /// The status cost observation the engine feeds before its UI sync: a settled record's spend is
    /// observed at mutation time, exactly once per settled record.
    pub fn observe_status_edges(
        &self,
        observe: Arc<dyn Fn(&StoreMutation) + Send + Sync>,
    ) -> MutationSubscription {
        self.on_mutation(Arc::new(move |change: &StoreMutation| {
            let settled = change.record.as_ref().is_some_and(|record| {
                record.status.is_terminal() && change.previous_status != Some(record.status)
            });
            if settled {
                observe(change);
            }
        }))
    }
}

pub fn create_task_store_chain(deps: TaskStoreChainDeps) -> TaskStoreChain {
    let observing = create_completion_observing_store(
        deps.base_store,
        CompletionBridgeDeps {
            notifier: deps.notifier,
            parent_state: deps.parent_state,
            was_background: deps.was_background,
            on_terminal: deps.on_terminal,
        },
    );
    let listeners: Arc<std::sync::Mutex<Vec<MutationListener>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let dispatch = Arc::clone(&listeners);
    let store = create_mutation_notifying_store(
        observing.backing_store(),
        Arc::new(move |change: StoreMutation| {
            for listener in dispatch
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
            {
                listener(&change);
            }
        }),
    );
    TaskStoreChain { store, listeners }
}

/// The terminal edge the status ledger records for one mutation.
pub fn terminal_edge(change: &StoreMutation) -> Option<(String, Option<TaskStatus>, TaskStatus)> {
    let settled = change.record.as_ref().is_some_and(|record| {
        record.status.is_terminal() && change.previous_status != Some(record.status)
    });
    settled.then(|| {
        (
            change.task_id.clone(),
            change.previous_status,
            change.record.as_ref().expect("settled record").status,
        )
    })
}

/// A record that reached a terminal status, for callers that only need the settled ones.
pub fn settled_record(change: &StoreMutation) -> Option<&TaskRecord> {
    terminal_edge(change).and(change.record.as_ref())
}
