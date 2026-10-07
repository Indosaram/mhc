//! Deterministic single-owner cancellation settlement (upstream `stopSettlement`,
//! `manager/manager-outcome.ts:38`).
//!
//! A manager-initiated cancel destroys the resident task, which forgets the live handle; the outcome
//! watcher's ownership re-check then fails and, without the pending-stop settlement, an isolated
//! child's `merge_result` is silently dropped. The cancel is registered as pending BEFORE the
//! steering call and settled on every path by a drop guard, so the watcher waits for the cancel and
//! still settles the isolation, leaving the cancel as the single terminal writer.
//!
//! Regression shape: this fails before the seam exists (the settle never lands) and passes after it.
//! It subscribes to the store's OWN mutation event (the write that publishes `merge_result`) BEFORE
//! the cancel, settles the fake handle's outcome itself (the fake's `abort` only counts), then waits
//! on that signal with a task-id predicate under one shared mutex, so it never polls and never races
//! the persistence it asserts. The state dir carries no baseline, so the settle resolves through the
//! independent baseline-unavailable path and never reaches the isolation runtime.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use isolation_core::{
    BackendKind, DeltaPatchResult, IsolationError, IsolationHandle, IsolationMergeOptions,
    IsolationMergeResult, IsolationOwner, OwnerProbe, SweepResult, WorktreeBaseline,
};

use crate::isolation::{EnsureInput, IsolationRuntime};
use crate::manager::isolation_wiring::IsolationSettings;
use crate::manager::manager_tests::fakes::{
    FakeRunner, HarnessOptions, Project, WAIT, base_spec, lock, make_manager,
};
use crate::manager::types::{StartResult, TaskManagerOptions};
use crate::runners::RunnerOutcome;
use crate::store::TaskRecordStore;
use crate::state::{
    IsolationMergeKind, IsolationMergeMode, IsolationRecord, TaskIsolationSpec, TaskRecord,
    TaskStatus,
};
use crate::steering::CancelOutcome;

/// Records every isolation call. `capture_delta` fails on purpose, so the settle resolves through the
/// baseline-unavailable path and lands a `retained` merge result without touching the filesystem.
#[derive(Default)]
struct RecordingRuntime {
    calls: Mutex<Vec<String>>,
}

impl RecordingRuntime {
    fn calls(&self) -> Vec<String> {
        lock(&self.calls).clone()
    }
}

impl IsolationRuntime for RecordingRuntime {
    fn resolve_repo_root(&self, _cwd: &Path) -> Option<PathBuf> {
        None
    }

    fn capture_baseline(&self, _repo_root: &Path) -> Result<WorktreeBaseline, IsolationError> {
        Err(IsolationError::other("unused in the cancellation regression"))
    }

    fn ensure(&self, _input: &EnsureInput) -> Result<IsolationHandle, IsolationError> {
        Err(IsolationError::other("unused in the cancellation regression"))
    }

    fn capture_delta(
        &self,
        _merged_dir: &Path,
        _baseline: &WorktreeBaseline,
    ) -> Result<DeltaPatchResult, IsolationError> {
        lock(&self.calls).push("capture_delta".to_string());
        Err(IsolationError::other("unused in the cancellation regression"))
    }

    fn merge(&self, _options: IsolationMergeOptions) -> Result<IsolationMergeResult, IsolationError> {
        lock(&self.calls).push("merge".to_string());
        Err(IsolationError::other("unused in the cancellation regression"))
    }

    fn cleanup(&self, _handle: &IsolationHandle) {}

    fn retain(&self, _handle: &IsolationHandle, _reason: &str) -> Result<PathBuf, IsolationError> {
        Err(IsolationError::other("unused in the cancellation regression"))
    }

    fn write_owner(&self, _base_dir: &Path, _id: &str, _owner: &IsolationOwner) {}

    fn sweep(
        &self,
        _roots: &[PathBuf],
        _probe: Arc<dyn OwnerProbe>,
    ) -> Result<SweepResult, IsolationError> {
        Ok(SweepResult::default())
    }

    fn sweep_roots(&self) -> Vec<PathBuf> {
        Vec::new()
    }
}

fn isolated_spec(base_dir: &Path) -> TaskIsolationSpec {
    TaskIsolationSpec {
        backend: BackendKind::Rcopy,
        fell_back: Some(false),
        merged_dir: base_dir.join("m").to_string_lossy().into_owned(),
        base_dir: base_dir.to_string_lossy().into_owned(),
        mode: IsolationMergeMode::Patch,
        apply: true,
    }
}

/// Blocks until the store PERSISTS `task_id`'s merge result. The store's mutation event is the
/// signal, and the callback and this waiter share one mutex, so a write landing between the
/// predicate read and the wait cannot be missed. Bounded by [`WAIT`]; no polling.
fn wait_for_merge_result(
    store: &TaskRecordStore,
    task_id: &str,
    signal: &Arc<(Mutex<()>, Condvar)>,
) -> TaskRecord {
    let deadline = Instant::now() + WAIT;
    let (mutex, wake) = &**signal;
    let mut guard = lock(mutex);
    loop {
        let record = store.load(task_id).expect("load").expect("record");
        if record
            .isolation
            .as_ref()
            .and_then(|isolation| isolation.merge_result.as_ref())
            .is_some()
        {
            return record;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "the cancelled run's merge result was never persisted"
        );
        guard = wake
            .wait_timeout(guard, remaining)
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0;
    }
}

#[test]
fn given_a_manager_cancel_of_an_isolated_run_when_the_watcher_settles_then_the_merge_result_lands_and_cancelled_stands()
{
    let project = Project::new();
    let runner = FakeRunner::new();
    let runtime = Arc::new(RecordingRuntime::default());
    let wired: Arc<dyn IsolationRuntime> = runtime.clone();
    let harness = make_manager(HarnessOptions {
        in_process: Some(runner.clone()),
        process: Some(runner.clone()),
        customize: Some(Box::new(move |options: &mut TaskManagerOptions| {
            options.isolation = Some(wired);
            options.isolation_settings = IsolationSettings {
                enabled: false,
                backend: Some(BackendKind::Rcopy),
                merge: IsolationMergeMode::Patch,
                apply: true,
            };
        })),
        ..HarnessOptions::default()
    });

    // A normal launch first (isolation is off at spawn time), so a real handle is tracked and the
    // outcome watcher is armed exactly as production arms it.
    let task_id = match harness.manager.start(&base_spec()) {
        StartResult::Started(task) => task.task_id,
        other => panic!("expected a started task, got {other:?}"),
    };
    let handle = runner.wait_handle(&task_id);

    // The run is isolated from here on: the persisted record carries the isolation spec with no
    // merge result, exactly the state a cancelled isolated child leaves behind.
    let base_dir = project.dir.path().join("sandbox");
    harness
        .store
        .mutate(&task_id, |record| TaskRecord {
            isolation: Some(IsolationRecord::new(isolated_spec(&base_dir))),
            ..record.clone()
        })
        .expect("isolation stamped on the record");
    assert!(
        harness
            .store
            .load(&task_id)
            .expect("load")
            .is_some_and(|record| record.isolation.is_some()),
        "the record must carry the isolation spec before the cancel"
    );

    // Subscribe to the store's own mutation event BEFORE the cancel: the merge result is published by
    // the settle's write, so the WRITE is the event to await, never the runtime call that precedes it.
    let signal = Arc::new((Mutex::new(()), Condvar::new()));
    let notifier = signal.clone();
    harness.store.set_mutation_listener(Some(Arc::new(move || {
        let _guard = lock(&notifier.0);
        notifier.1.notify_all();
    })));
    let outcome = harness
        .manager
        .cancel_task(&task_id, Some("regression cancel"), Default::default())
        .expect("cancel applied");
    assert!(
        matches!(outcome, CancelOutcome::Cancelled { .. }),
        "the cancel must be applied from running, got {outcome:?}"
    );
    // The fake's `abort` only counts; the watcher needs the handle's real outcome to run.
    handle.settle(RunnerOutcome::Cancelled);
    let record = wait_for_merge_result(&harness.store, &task_id, &signal);
    harness.store.set_mutation_listener(None);
    assert_eq!(
        record.status,
        TaskStatus::Cancelled,
        "the cancel is the single terminal writer; the watcher must not overwrite it"
    );
    let merge = record
        .isolation
        .as_ref()
        .and_then(|isolation| isolation.merge_result.as_ref())
        .expect("the cancelled run's merge result");
    assert_eq!(merge.kind, IsolationMergeKind::Retained, "a cancel never merges");
    assert!(!merge.changes_applied);
    assert_eq!(merge.reason.as_deref(), Some("child did not complete"));
    // No baseline is persisted for this task, so the settle lands the independent missing-baseline
    // result and never touches the runtime.
    assert!(merge.patch_path.is_none());
    assert!(
        merge.error.as_deref().is_some_and(|error| error.contains("baseline is unavailable")),
        "expected the missing-baseline retained result, got {:?}",
        merge.error
    );
    assert!(
        runtime.calls().is_empty(),
        "the missing-baseline settle must not reach the runtime, calls: {:?}",
        runtime.calls()
    );
    harness.manager.forget(&task_id);
}
