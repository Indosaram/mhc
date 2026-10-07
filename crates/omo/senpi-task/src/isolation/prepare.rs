//! isolation/prepare.ts: build the child's sandbox BEFORE the record is committed to a launch.
//!
//! A repository that cannot be cloned must refuse the spawn, never silently run the child against
//! the parent checkout, because isolated is a safety promise the caller relies on.

use std::path::Path;

use isolation_core::{HostOwner, IsolationHandle, IsolationOwner, WorktreeBaseline};

use crate::isolation::baseline_store::write_baseline;
use crate::isolation::runtime::{EnsureInput, IsolationRuntime};
use crate::state::{IsolationMergeMode, TaskIsolationSpec};

/// The result of a prepare attempt (TS IsolationPreparation).
#[derive(Debug, Clone, PartialEq)]
pub enum IsolationPreparation {
    /// The sandbox exists and the baseline is persisted.
    Prepared {
        handle: IsolationHandle,
        baseline: WorktreeBaseline,
        spec: TaskIsolationSpec,
    },
    /// The spawn must be refused with this reason.
    Refused { reason: String },
}

/// Input of prepare_isolation (TS PrepareIsolationInput).
pub struct PrepareIsolationInput<'a> {
    pub runtime: &'a dyn IsolationRuntime,
    pub cwd: &'a Path,
    pub task_id: &'a str,
    pub state_dir: &'a Path,
    /// 'None' is the config's "auto": the producer picks the backend.
    pub backend: Option<isolation_core::BackendKind>,
    pub mode: IsolationMergeMode,
    pub apply: bool,
    pub host_pid: i64,
}

/// Build the child's sandbox before the launch is committed.
pub fn prepare_isolation(input: &PrepareIsolationInput<'_>) -> IsolationPreparation {
    let Some(repo_root) = input.runtime.resolve_repo_root(input.cwd) else {
        return IsolationPreparation::Refused {
            reason: "not a git checkout".to_string(),
        };
    };

    let baseline = match input.runtime.capture_baseline(&repo_root) {
        Ok(baseline) => baseline,
        Err(error) => {
            return IsolationPreparation::Refused {
                reason: format!("baseline capture failed: {error}"),
            };
        }
    };

    let owner = IsolationOwner {
        host: HostOwner {
            pid: u32::try_from(input.host_pid).unwrap_or_else(|_| std::process::id()),
        },
        child: None,
    };
    let handle = match input.runtime.ensure(&EnsureInput {
        repo_root: repo_root.clone(),
        id: input.task_id.to_string(),
        preferred: input.backend,
        owner: Some(owner),
    }) {
        Ok(handle) => handle,
        Err(error) => {
            return IsolationPreparation::Refused {
                reason: error.to_string(),
            };
        }
    };

    if let Err(error) = write_baseline(input.state_dir, input.task_id, &baseline) {
        input.runtime.cleanup(&handle);
        return IsolationPreparation::Refused {
            reason: format!("baseline could not be persisted: {error}"),
        };
    }

    IsolationPreparation::Prepared {
        spec: TaskIsolationSpec {
            backend: handle.backend,
            fell_back: Some(handle.fell_back),
            merged_dir: handle.merged_dir.to_string_lossy().into_owned(),
            base_dir: handle.base_dir.to_string_lossy().into_owned(),
            mode: input.mode,
            apply: input.apply,
        },
        handle,
        baseline,
    }
}
