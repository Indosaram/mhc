//! Workflow runs: dependency graphs of delegated tasks (MIT, clean room; see `PROVENANCE.md`).
//!
//! Behaviour spec: `docs/cleanroom/specs/workflow-dag.md`.
//!
//! - [`definition`]: the `{key, name, nodes[]}` definition, its validation codes and fingerprint.
//! - [`graph`]: derived waves, edges, critical path and bottlenecks.
//! - [`state`]: run and node states, node error codes, the persisted run record.
//! - [`store`]: the `workflows/` directory (key index, run records, journals, results, leases)
//!   and the retention sweep.
//! - [`executor`]: how a node's child task is started, awaited, steered and cancelled.
//! - [`scheduler`]: the frontier scheduler, start, settle, retries, durability and recovery.
//! - [`scheduler_ops`]: snapshot, attach, wait, cancel, retry, send, amend, guidance and the
//!   user pause/resume control.
//! - [`tool`]: the `workflow` tool actions and their result shapes.
//! - [`dag`]: the `/dag` command text.
//! - [`addon`]: the `maho-task` addon that registers the tool and command on top of the task
//!   runner.
pub mod addon;
pub mod dag;
pub mod definition;
pub mod executor;
pub mod graph;
pub mod scheduler;
pub mod scheduler_ops;
pub mod settings;
pub mod skills;
pub mod state;
pub mod store;
pub mod tool;

pub use addon::{TaskExecutor, WorkflowAddon};
pub use definition::{ValidationIssue, WorkflowDefinition, WorkflowNode, validate};
pub use executor::{ChildOutcome, ChildProbe, NodeExecutor, NodeRequest, SendOutcome, StartError};
pub use graph::GraphInfo;
pub use scheduler::{RunEvent, SchedulerOptions, StartOutcome, WorkflowError, WorkflowScheduler};
pub use scheduler_ops::{NodeResult, Snapshot};
pub use settings::DagSettings;
pub use skills::SkillResolver;
pub use state::{NodeError, NodeRecord, NodeState, RunStatus, WorkflowRun};
pub use store::WorkflowStore;
pub use tool::{Caller, TOOL_NAME, ToolOutcome, UPDATE_MESSAGE_TYPE};

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

pub(crate) fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
