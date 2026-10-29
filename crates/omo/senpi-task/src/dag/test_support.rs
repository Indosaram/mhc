//! Shared fixtures for the `dag/` test modules: the TS suites settle a node's task by node id
//! because their fake manager's task id *is* the node id, while the Rust port runs the real
//! `TaskManager` and must resolve the id the scheduler persisted in `NodeTaskAttached` instead.

use std::sync::Arc;

use crate::dag::manager::DagRunRecordV1;
use crate::dag::store::DagFileStore;
use crate::dag::types::DagRunId;
use crate::manager::manager_tests::fakes::{FakeHandle, FakeRunner, wait_until};

/// The task id the scheduler attached to `node_id` (see the module note for why the node id alone
/// cannot be used here).
pub(crate) fn attached_task_id(
    store: &DagFileStore,
    run_id: &DagRunId,
    node_id: &str,
) -> Option<String> {
    store
        .read_checkpoint::<DagRunRecordV1>(run_id)
        .ok()
        .flatten()
        .and_then(|record| {
            record
                .nodes
                .iter()
                .find(|node| node.id == node_id)
                .and_then(|node| node.task_id.clone())
        })
}

pub(crate) fn wait_for_attached_handle(
    store: &DagFileStore,
    runner: &Arc<FakeRunner>,
    run_id: &DagRunId,
    node_id: &str,
) -> Arc<FakeHandle> {
    wait_until(&format!("runner handle for node {node_id}"), || {
        attached_task_id(store, run_id, node_id).is_some_and(|id| runner.handle(&id).is_some())
    });
    let task_id = attached_task_id(store, run_id, node_id).expect("task id resolved");
    runner.handle(&task_id).expect("handle present")
}

/// The TS fixture's default `autoComplete: true`: settle each node's task as soon as the scheduler
/// attaches it, so a blocking `run()` reaches a terminal record on its own. Join the handles after
/// `run()` returns so no completion is left in flight when a test asserts.
pub(crate) fn spawn_auto_complete(
    store: &Arc<DagFileStore>,
    runner: &Arc<FakeRunner>,
    run_id: DagRunId,
    node_id: &'static str,
) -> std::thread::JoinHandle<()> {
    let store = Arc::clone(store);
    let runner = Arc::clone(runner);
    std::thread::spawn(move || {
        let handle = wait_for_attached_handle(&store, &runner, &run_id, node_id);
        handle.complete(&format!("done {node_id}"));
    })
}

pub(crate) fn auto_complete_nodes(
    store: &Arc<DagFileStore>,
    runner: &Arc<FakeRunner>,
    run_id: &DagRunId,
    node_ids: &[&'static str],
) -> Vec<std::thread::JoinHandle<()>> {
    node_ids
        .iter()
        .map(|node_id| spawn_auto_complete(store, runner, run_id.clone(), node_id))
        .collect()
}

pub(crate) fn join_all(handles: Vec<std::thread::JoinHandle<()>>) {
    for handle in handles {
        handle.join().expect("auto-complete thread");
    }
}
