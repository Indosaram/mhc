//! Interfaces for the parts of delegation that later phases implement: workspace isolation
//! (worktrees and similar, Phase 3) and team/terminal-pane presentation (Phase 3). This crate only
//! calls them; it links no implementation.
use crate::record::{StateDir, TaskRecord};
use crate::runner::BoxFuture;
use std::path::{Path, PathBuf};

/// A prepared isolated workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IsolatedWorkspace {
    /// Where the child runs.
    pub cwd: PathBuf,
    /// Backend-specific handle (branch name, snapshot id, ...).
    pub handle: Option<String>,
}

/// Gives a child its own copy of the workspace and integrates (or keeps) its changes afterwards.
pub trait WorkspaceIsolation: Send + Sync {
    fn name(&self) -> &str;
    fn prepare<'a>(
        &'a self,
        task_id: &'a str,
        cwd: &'a Path,
    ) -> BoxFuture<'a, Result<IsolatedWorkspace, String>>;
    /// Called once the run ended; `Some(text)` summarises what happened to the changes.
    fn finish<'a>(
        &'a self,
        workspace: &'a IsolatedWorkspace,
        succeeded: bool,
    ) -> BoxFuture<'a, Result<Option<String>, String>>;
}

/// Observes task lifecycle (a team view or terminal panes can follow children with it).
/// The manager calls the `_in` forms, which also name the task's state directory (output, log);
/// their default bodies forward to `started`/`finished`.
pub trait TaskObserver: Send + Sync {
    fn started(&self, _record: &TaskRecord) {}
    fn finished(&self, _record: &TaskRecord) {}
    fn started_in(&self, record: &TaskRecord, _state: &StateDir) {
        self.started(record);
    }
    fn finished_in(&self, record: &TaskRecord, _state: &StateDir) {
        self.finished(record);
    }
}
