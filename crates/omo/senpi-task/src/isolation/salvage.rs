//! isolation/salvage.ts: crash salvage and the startup sweep of dead clones.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use isolation_core::{IsolationError, OwnerProbe, SweepResult};

use crate::isolation::runtime::IsolationRuntime;
use crate::isolation::settle::{SettleIsolationInput, settle_isolation};
use crate::state::{IsolationRecord, TaskRecord};

/// The ports a salvage pass writes through (TS SalvagePorts).
pub struct SalvagePorts<'a> {
    pub runtime: &'a dyn IsolationRuntime,
    pub state_dir: &'a Path,
    pub mutate: &'a dyn Fn(&str, &dyn Fn(&TaskRecord) -> TaskRecord),
}

/// A host that died mid-run left an isolation with no merge result on a terminal record.
pub fn needs_crash_salvage(record: &TaskRecord, terminal: &dyn Fn(&TaskRecord) -> bool) -> bool {
    record
        .isolation
        .as_ref()
        .is_some_and(|isolation| isolation.merge_result.is_none())
        && terminal(record)
}

/// A host that died mid-run never got to judge the child's work, so its delta is captured as
/// artifacts and NEVER auto-merged: replaying an unreviewed child's edits into the checkout on the
/// next startup is exactly the surprise isolation exists to prevent.
pub fn salvage_crashed_isolation(ports: &SalvagePorts<'_>, record: &TaskRecord) {
    let Some(isolation) = record.isolation.as_ref() else {
        return;
    };
    let merge_result = settle_isolation(&SettleIsolationInput {
        runtime: ports.runtime,
        state_dir: ports.state_dir,
        task_id: &record.task_id,
        isolation: &isolation.spec,
        merge: false,
        reason: Some("host_crashed"),
        baseline: None,
        handle: None,
    });
    (ports.mutate)(&record.task_id, &|fresh: &TaskRecord| match fresh.isolation.as_ref() {
        None => fresh.clone(),
        Some(current) => TaskRecord {
            isolation: Some(IsolationRecord {
                spec: current.spec.clone(),
                merge_result: Some(merge_result.clone()),
            }),
            ..fresh.clone()
        },
    });
}

/// The sweep roots: this runtime's own roots plus every base dir seen on a record.
pub fn sweep_roots_for(runtime: &dyn IsolationRuntime, records: &[TaskRecord]) -> Vec<PathBuf> {
    let mut roots: BTreeSet<PathBuf> = runtime.sweep_roots().into_iter().collect();
    for record in records {
        if let Some(isolation) = record.isolation.as_ref()
            && let Some(parent) = Path::new(&isolation.spec.base_dir).parent()
        {
            roots.insert(parent.to_path_buf());
        }
    }
    roots.into_iter().collect()
}

/// Sweep every dead clone under the resolved roots.
pub fn sweep_isolations(
    runtime: &dyn IsolationRuntime,
    records: &[TaskRecord],
    probe: Arc<dyn OwnerProbe>,
) -> Result<SweepResult, IsolationError> {
    let roots = sweep_roots_for(runtime, records);
    runtime.sweep(&roots, probe)
}
