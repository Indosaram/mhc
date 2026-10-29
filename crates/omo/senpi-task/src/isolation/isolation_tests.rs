//! Producer-independent contract tests for the isolation CONSUMER.
//!
//! These exercise the consumer orchestration (prepare refusal / settle / salvage / details) against a
//! FakeIsolationRuntime that performs NO real cloning, so the contract is pinned without a real
//! sandbox. The fake implements the port with canned values and records the calls it received,
//! mirroring packages/senpi-task/src/manager/__fixtures__/isolation-fakes.ts.
//!
//! NOTE: the producer's IsolationHandle has PRIVATE stop/relocate fields, so a fake cannot construct
//! one. The fake therefore refuses `ensure`; handle-dependent consumer tests (a real prepared
//! sandbox, retain/cleanup of a real handle) belong to integration and are recorded in the receipt,
//! not faked here. Everything below needs no IsolationHandle.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use isolation_core::{
    BackendKind, DeltaPatchResult, IsolationError, IsolationHandle, IsolationMergeOptions,
    IsolationMergeResult, IsolationOwner, MergeKind, MergeState, OwnerProbe, RepoBaseline,
    SweepResult, WorktreeBaseline,
};

use crate::isolation::details::{isolation_details, isolation_line};
use crate::isolation::prepare::{IsolationPreparation, PrepareIsolationInput, prepare_isolation};
use crate::isolation::runtime::{EnsureInput, IsolationRuntime};
use crate::isolation::salvage::{needs_crash_salvage, sweep_roots_for};
use crate::isolation::settle::{SettleIsolationInput, settle_isolation};
use crate::manager::isolation_wiring::IsolationSettings;
use crate::state::{
    IsolationMergeMode, IsolationMergeResult as TaskMergeResult, IsolationRecord, TaskIsolationSpec,
    TaskRecord, TaskStatus, create_task_record,
};

/// A fake runtime that performs no cloning and records the calls it received.
#[derive(Default)]
struct FakeRuntime {
    repo_root: Option<PathBuf>,
    baseline_fails: bool,
    merge_fails: bool,
    calls: Mutex<Vec<String>>,
    roots: Vec<PathBuf>,
}

impl FakeRuntime {
    fn recording(&self, call: &str) {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(call.to_string());
    }

    fn calls(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

fn baseline(repo_root: &Path) -> WorktreeBaseline {
    WorktreeBaseline {
        root: RepoBaseline {
            repo_root: repo_root.to_path_buf(),
            head_commit: "deadbeef".to_string(),
            staged: String::new(),
            unstaged: String::new(),
            untracked_files: Vec::new(),
            untracked_patch: String::new(),
        },
        nested: Vec::new(),
    }
}

impl IsolationRuntime for FakeRuntime {
    fn resolve_repo_root(&self, _cwd: &Path) -> Option<PathBuf> {
        self.recording("resolve_repo_root");
        self.repo_root.clone()
    }

    fn capture_baseline(&self, repo_root: &Path) -> Result<WorktreeBaseline, IsolationError> {
        self.recording("capture_baseline");
        if self.baseline_fails {
            return Err(IsolationError::other("baseline boom"));
        }
        Ok(baseline(repo_root))
    }

    fn ensure(&self, _input: &EnsureInput) -> Result<IsolationHandle, IsolationError> {
        // The producer's IsolationHandle has private fields, so a fake cannot build one.
        self.recording("ensure");
        Err(IsolationError::other("fake runtime does not build a real handle"))
    }

    fn capture_delta(
        &self,
        _merged_dir: &Path,
        _baseline: &WorktreeBaseline,
    ) -> Result<DeltaPatchResult, IsolationError> {
        self.recording("capture_delta");
        Ok(DeltaPatchResult { root_patch: "patch".to_string(), nested_patches: Vec::new() })
    }

    fn merge(&self, _options: IsolationMergeOptions) -> Result<IsolationMergeResult, IsolationError> {
        self.recording("merge");
        if self.merge_fails {
            return Err(IsolationError::other("merge boom"));
        }
        Ok(IsolationMergeResult {
            state: MergeState::new(MergeKind::Applied, true),
            patch_path: Some("patch.diff".to_string()),
            nested_patch_paths: None,
            summary_path: "summary.md".to_string(),
            files_changed: 1,
        })
    }

    fn cleanup(&self, _handle: &IsolationHandle) {
        self.recording("cleanup");
    }

    fn retain(&self, handle: &IsolationHandle, reason: &str) -> Result<PathBuf, IsolationError> {
        self.recording(&format!("retain:{reason}"));
        Ok(PathBuf::from(format!("{}.retained-1", handle.base_dir.display())))
    }

    fn write_owner(&self, _base_dir: &Path, _id: &str, _owner: &IsolationOwner) {
        self.recording("write_owner");
    }

    fn sweep(&self, roots: &[PathBuf], _probe: Arc<dyn OwnerProbe>) -> Result<SweepResult, IsolationError> {
        self.recording(&format!("sweep:{}", roots.len()));
        Ok(SweepResult::default())
    }

    fn sweep_roots(&self) -> Vec<PathBuf> {
        self.roots.clone()
    }
}

fn spec() -> TaskIsolationSpec {
    TaskIsolationSpec {
        backend: BackendKind::Rcopy,
        fell_back: Some(false),
        merged_dir: "/iso/t1/m".to_string(),
        base_dir: "/iso/t1".to_string(),
        mode: IsolationMergeMode::Patch,
        apply: true,
    }
}

fn record_with(isolation: Option<IsolationRecord>) -> TaskRecord {
    let input = crate::state::TaskRecordInput {
        parent_session_id: "parent".to_string(),
        root_session_id: "parent".to_string(),
        depth: 1,
        execution_mode: "in-process".to_string(),
        model: "offline/offline".to_string(),
        ..Default::default()
    };
    let record = create_task_record(input, Some(0)).expect("record");
    TaskRecord {
        status: TaskStatus::Completed,
        isolation,
        ..record
    }
}

#[test]
fn prepare_refuses_a_non_git_checkout_without_touching_the_child_cwd() {
    let runtime = FakeRuntime::default();
    let dir = tempfile::tempdir().expect("tempdir");
    let preparation = prepare_isolation(&PrepareIsolationInput {
        runtime: &runtime,
        cwd: dir.path(),
        task_id: "st_00000001",
        state_dir: dir.path(),
        backend: Some(BackendKind::Rcopy),
        mode: IsolationMergeMode::Patch,
        apply: true,
        host_pid: 1,
    });
    assert!(matches!(preparation, IsolationPreparation::Refused { reason } if reason == "not a git checkout"));
    assert_eq!(runtime.calls(), vec!["resolve_repo_root".to_string()]);
}

#[test]
fn prepare_refuses_when_the_baseline_cannot_be_captured() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = FakeRuntime {
        repo_root: Some(dir.path().to_path_buf()),
        baseline_fails: true,
        ..FakeRuntime::default()
    };
    let preparation = prepare_isolation(&PrepareIsolationInput {
        runtime: &runtime,
        cwd: dir.path(),
        task_id: "st_00000001",
        state_dir: dir.path(),
        backend: Some(BackendKind::Rcopy),
        mode: IsolationMergeMode::Patch,
        apply: true,
        host_pid: 1,
    });
    assert!(matches!(preparation, IsolationPreparation::Refused { ref reason } if reason.starts_with("baseline capture failed")));
}

#[test]
fn settle_merges_a_completed_child_and_projects_the_result() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = FakeRuntime::default();
    let isolation = spec();
    let baseline = baseline(dir.path());
    let result = settle_isolation(&SettleIsolationInput {
        runtime: &runtime,
        state_dir: dir.path(),
        task_id: "st_00000001",
        isolation: &isolation,
        merge: true,
        reason: None,
        baseline: Some(&baseline),
        handle: None,
    });
    assert_eq!(result.kind, MergeKind::Applied);
    assert!(result.changes_applied);
    assert!(runtime.calls().contains(&"merge".to_string()));
}

#[test]
fn settle_retains_when_the_merge_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = FakeRuntime { merge_fails: true, ..FakeRuntime::default() };
    let isolation = spec();
    let baseline = baseline(dir.path());
    let result = settle_isolation(&SettleIsolationInput {
        runtime: &runtime,
        state_dir: dir.path(),
        task_id: "st_00000001",
        isolation: &isolation,
        merge: true,
        reason: Some("host_crashed"),
        baseline: Some(&baseline),
        handle: None,
    });
    assert_eq!(result.kind, MergeKind::Retained);
    assert!(!result.changes_applied);
    assert_eq!(result.reason.as_deref(), Some("host_crashed"));
}

#[test]
fn needs_crash_salvage_is_true_only_for_a_terminal_isolated_record_without_a_merge_result() {
    let terminal = |record: &TaskRecord| record.status.is_terminal();
    let bare = record_with(Some(IsolationRecord::new(spec())));
    assert!(needs_crash_salvage(&bare, &terminal));
    let merged = record_with(Some(IsolationRecord {
        merge_result: Some(TaskMergeResult::retained(None, None, 1)),
        ..IsolationRecord::new(spec())
    }));
    assert!(!needs_crash_salvage(&merged, &terminal));
    assert!(!needs_crash_salvage(&record_with(None), &terminal));
}

#[test]
fn sweep_roots_union_the_runtime_roots_with_every_record_base_dir() {
    let runtime = FakeRuntime { roots: vec![PathBuf::from("/home/.omo/wt")], ..FakeRuntime::default() };
    let record = record_with(Some(IsolationRecord::new(spec())));
    let roots = sweep_roots_for(&runtime, std::slice::from_ref(&record));
    assert!(roots.contains(&PathBuf::from("/home/.omo/wt")));
    assert!(roots.contains(&PathBuf::from("/iso")));
}

#[test]
fn isolation_details_and_line_project_the_persisted_facts() {
    let record = record_with(Some(IsolationRecord {
        merge_result: Some(TaskMergeResult::retained(None, None, 1)),
        ..IsolationRecord::new(spec())
    }));
    let details = isolation_details(&record).expect("details");
    assert_eq!(details.backend, "rcopy");
    assert_eq!(isolation_line(&details), "isolation: retained via rcopy");
    assert!(!details.changes_applied);
}

#[test]
fn isolation_settings_from_config_keeps_auto_as_no_preference() {
    let auto = IsolationSettings::from_config(&serde_json::json!({
        "isolation": { "enabled": true, "backend": "auto", "apply": true, "merge": "patch" }
    }));
    assert_eq!(auto.backend, None, "the config sentinel 'auto' is not a backend");
    assert!(auto.enabled);
    assert!(auto.apply);
    let pinned = IsolationSettings::from_config(&serde_json::json!({
        "isolation": { "enabled": true, "backend": "rcopy", "apply": false, "merge": "branch" }
    }));
    assert_eq!(pinned.backend, Some(BackendKind::Rcopy));
    assert!(!pinned.apply);
    assert_eq!(pinned.merge, IsolationMergeMode::Branch);
}

#[test]
fn isolation_settings_defaults_match_the_config_schema() {
    let default = IsolationSettings::from_config(&serde_json::json!({}));
    assert_eq!(default, IsolationSettings::default());
    assert!(!default.enabled);
    assert_eq!(default.backend, None, "the schema default backend is 'auto'");
    assert!(default.apply, "the schema default for apply is true");
    assert_eq!(default.merge, IsolationMergeMode::Patch);
}
