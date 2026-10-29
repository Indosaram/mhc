use std::path::Path;
use memory_core::{
    identity::layout::build_identity_paths,
    identity::resolve::MemoryIdentity,
    receipts::{read_memory_receipts, MemoryReceipt, MemoryReceiptInput, ReadMemoryReceiptsOptions},
};
use crate::{
    receipts_port::MemoryReceiptsPort,
    worker::{
        reservation_run_ledger::{parse_reservation_run_ledger, ReservationRunLedger},
        run_finalization_types::RunFinalizationContext,
        run_artifacts::write_run_json_atomic,
    },
};


fn test_ledger(overrides: Option<serde_json::Value>) -> (ReservationRunLedger, serde_json::Value) {
    let mut val = serde_json::json!({
        "version": 1,
        "runId": "run-1",
        "category": "deep",
        "conversationIds": ["conversation-a"],
        "kind": "reflection",
        "trigger": "step-count",
        "startedAt": "2026-08-11T10:00:00.000Z",
        "hardDeadlineAt": 1,
        "terminationGraceMs": 1,
        "deadlineAt": 2,
        "mergePolicy": "auto",
        "worktreeDir": "/tmp/worktree",
        "worktreeBranch": "memory/reflection-run-1",
        "baseSha": "base",
        "gitFilePath": "/tmp/worktree/.git",
        "gitFileSnapshot": "gitdir: x\n",
        "commonConfigPath": "/tmp/config",
        "commonConfigSnapshot": null,
    });
    if let Some(overrides) = overrides
        && let Some(obj) = overrides.as_object() {
            for (k, v) in obj {
                val[k] = v.clone();
            }
    }
    let parsed = parse_reservation_run_ledger(val.clone()).unwrap();
    (parsed, val)
}

fn identity_with_run(root: &Path, run_id: &str, write_ledger: bool) -> (MemoryIdentity, std::path::PathBuf) {
    let paths = build_identity_paths(root, "agent-test");
    let identity = MemoryIdentity {
        id: "agent-test".into(),
        safe_slug: "agent-test".into(),
        paths,
    };
    let run_dir = identity.paths.reflection.join("runs").join(run_id);
    std::fs::create_dir_all(&run_dir).unwrap();
    if write_ledger {
        let (_, val) = test_ledger(Some(serde_json::json!({ "runId": run_id })));
        write_run_json_atomic(&run_dir.join("ledger.json"), &val, 0o600).unwrap();
    }
    (identity, run_dir)
}

struct IdleReservation;
impl crate::worker::runner_types::ReflectionReservationPort for IdleReservation {
    fn read_state(&self) -> Result<memory_core::reflection::ReservationState, String> {
        Ok(Default::default())
    }
    fn complete(&self, _run_id: &str, outcome: memory_core::reflection::ReflectionOutcome) -> Result<memory_core::reflection::CompletionResult, String> {
        Ok(memory_core::reflection::CompletionResult {
            launch: None,
            outcome,
        })
    }
}

fn read_receipts(runtime_dir: &Path) -> Vec<MemoryReceipt> {
    read_memory_receipts(runtime_dir, &ReadMemoryReceiptsOptions::default()).unwrap().receipts
}

#[test]
fn given_a_merged_settlement_when_final_json_lands_then_one_merged_receipt_with_the_integration_sha_follows_it() {
    let root = tempfile::tempdir().unwrap();
    let (identity, run_dir) = identity_with_run(root.path(), "run-1", true);
    let (ledger, _) = test_ledger(None);

    let calls = std::sync::Mutex::new(Vec::new());
    struct OrderPort<'a> {
        run_dir: &'a Path,
        calls: &'a std::sync::Mutex<Vec<(String, bool, bool)>>,
    }
    impl MemoryReceiptsPort for OrderPort<'_> {
        fn append(&self, runtime_dir: &Path, input: &MemoryReceiptInput) -> Result<bool, String> {
            let present = self.run_dir.join("final.json").exists() || self.run_dir.join("abandoned.json").exists();
            self.calls.lock().unwrap().push((input.event().to_string(), present, false));
            memory_core::receipts::append_memory_receipt_once(runtime_dir, input).map_err(|e| e.to_string())
        }
    }

    let port = OrderPort { run_dir: &run_dir, calls: &calls };
    let res = IdleReservation;
    let now_fn = || 1723370460000;
    let context = RunFinalizationContext {
        identity: &identity,
        reservation: &res,
        launch: None,
        now_ms: &now_fn,
        receipts: Some(&port),
        warn: None,
    };

    crate::worker::run_finalization_settlement::settle_reservation_run(
        &context,
        &run_dir,
        &ledger,
        crate::worker::run_finalization_types::DurableFinalizationDecision { outcome: "merged".into(), reason: None, detail: None, integration_sha: Some("abc123".into()), recovered_from_worktree: false },
    ).unwrap();

    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded, vec![("merged".into(), true, false)]);
    let receipts = read_receipts(&identity.paths.runtime);
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].input.event(), "merged");
    assert_eq!(receipts[0].input.sha(), Some("abc123"));
}

#[test]
fn given_no_change_and_failed_settlements_when_settled_then_no_changes_and_failed_are_receipted() {
    let root = tempfile::tempdir().unwrap();
    let (identity, run_dir1) = identity_with_run(root.path(), "run-1", true);
    let run_dir2 = identity.paths.reflection.join("runs").join("run-2");
    std::fs::create_dir_all(&run_dir2).unwrap();
    let (ledger1, _) = test_ledger(Some(serde_json::json!({ "runId": "run-1" })));
    let (ledger2, _) = test_ledger(Some(serde_json::json!({ "runId": "run-2" })));

    let res = IdleReservation;
    let now_fn = || 1723370460000;
    let context = RunFinalizationContext {
        identity: &identity,
        reservation: &res,
        launch: None,
        now_ms: &now_fn,
        receipts: None,
        warn: None,
    };

    crate::worker::run_finalization_settlement::settle_reservation_run(
        &context,
        &run_dir1,
        &ledger1,
        crate::worker::run_finalization_types::DurableFinalizationDecision { outcome: "no_changes".into(), reason: None, detail: None, integration_sha: None, recovered_from_worktree: false },
    ).unwrap();

    crate::worker::run_finalization_settlement::settle_reservation_run(
        &context,
        &run_dir2,
        &ledger2,
        crate::worker::run_finalization_types::DurableFinalizationDecision { outcome: "failed".into(), reason: Some("validation_failed".into()), detail: Some("secret token=abc123456 leaked".into()), integration_sha: None, recovered_from_worktree: false },
    ).unwrap();

    let list = read_receipts(&identity.paths.runtime);
    assert_eq!(list.len(), 2);
    let events: Vec<_> = list.iter().map(|r| r.input.event()).collect();
    assert!(events.contains(&"no_changes"));
    assert!(events.contains(&"failed"));
    let failed_receipt = list.iter().find(|r| r.input.event() == "failed").unwrap();
    assert_eq!(failed_receipt.input.reason(), Some("validation_failed"));
    assert!(!failed_receipt.input.detail().unwrap_or("").contains("abc123456"));
}

#[test]
fn given_a_dead_run_nobody_can_finish_when_abandoned_then_abandoned_receipt_follows_it() {
    let root = tempfile::tempdir().unwrap();
    let (identity, run_dir) = identity_with_run(root.path(), "run-1", true);
    let (ledger, _) = test_ledger(None);

    let res = IdleReservation;
    let now_fn = || 1723370460000;
    let context = RunFinalizationContext {
        identity: &identity,
        reservation: &res,
        launch: None,
        now_ms: &now_fn,
        receipts: None,
        warn: None,
    };

    crate::worker::run_finalization::abandon_reservation_run(
        &context,
        &run_dir,
        &ledger,
        |_pid, _start| crate::worker::run_liveness::RunProcessVerdict::Dead,
        |op| op(),
    ).unwrap();

    assert!(run_dir.join("abandoned.json").exists());
    let list = read_receipts(&identity.paths.runtime);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].input.event(), "abandoned");
}

#[test]
fn final_sentinel_without_ledger_does_not_backfill_a_run_receipt() {
    let root = tempfile::tempdir().unwrap();
    let run_dir = root.path().join("run");
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(run_dir.join("final.json"), serde_json::json!({"outcome":"merged","kind":"dream","runId":"run","trigger":"idle","generation":"g1"}).to_string()).unwrap();
    crate::worker::run_receipt_backfill::backfill_run_receipt_sync(root.path(), &run_dir, None, None);
    assert!(read_receipts(root.path()).is_empty());
}

#[test]
fn given_a_merged_run_whose_receipt_write_failed_when_reconciled_twice_then_exactly_one_merged_receipt_exists() {
    let root = tempfile::tempdir().unwrap();
    let (identity, run_dir) = identity_with_run(root.path(), "run-1", true);
    let (ledger, _) = test_ledger(None);

    struct FailingPort;
    impl MemoryReceiptsPort for FailingPort {
        fn append(&self, _runtime_dir: &Path, _input: &MemoryReceiptInput) -> Result<bool, String> {
            Err("failed write".into())
        }
    }

    let res = IdleReservation;
    let now_fn = || 1723370460000;
    let context = RunFinalizationContext {
        identity: &identity,
        reservation: &res,
        launch: None,
        now_ms: &now_fn,
        receipts: Some(&FailingPort),
        warn: None,
    };

    crate::worker::run_finalization_settlement::settle_reservation_run(
        &context,
        &run_dir,
        &ledger,
        crate::worker::run_finalization_types::DurableFinalizationDecision { outcome: "merged".into(), reason: None, detail: None, integration_sha: Some("abc123".into()), recovered_from_worktree: false },
    ).unwrap();

    assert_eq!(read_receipts(&identity.paths.runtime).len(), 0);
    assert!(run_dir.join("final.json").exists());

    // Backfill sync
    crate::worker::run_receipt_backfill::backfill_run_receipt_sync(&identity.paths.runtime, &run_dir, None, None);
    crate::worker::run_receipt_backfill::backfill_run_receipt_sync(&identity.paths.runtime, &run_dir, None, None);

    let list = read_receipts(&identity.paths.runtime);
    let merged: Vec<_> = list.iter().filter(|r| r.input.event() == "merged").collect();
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].input.sha(), Some("abc123"));
}

#[test]
fn given_a_normally_receipted_run_when_reconciled_after_a_restart_then_no_second_merged_line_appears() {
    let root = tempfile::tempdir().unwrap();
    let (identity, run_dir) = identity_with_run(root.path(), "run-1", true);
    let (ledger, _) = test_ledger(None);

    let res = IdleReservation;
    let now_fn = || 1723370460000;
    let context = RunFinalizationContext {
        identity: &identity,
        reservation: &res,
        launch: None,
        now_ms: &now_fn,
        receipts: None,
        warn: None,
    };

    crate::worker::run_finalization_settlement::settle_reservation_run(
        &context,
        &run_dir,
        &ledger,
        crate::worker::run_finalization_types::DurableFinalizationDecision { outcome: "merged".into(), reason: None, detail: None, integration_sha: Some("abc123".into()), recovered_from_worktree: false },
    ).unwrap();

    assert_eq!(read_receipts(&identity.paths.runtime).len(), 1);

    crate::worker::run_receipt_backfill::backfill_run_receipt_sync(&identity.paths.runtime, &run_dir, None, None);
    crate::worker::run_receipt_backfill::backfill_run_receipt_sync(&identity.paths.runtime, &run_dir, None, None);

    let list = read_receipts(&identity.paths.runtime);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].input.event(), "merged");
}

#[test]
fn given_a_run_dir_reused_by_a_fresh_generation_when_each_generation_is_reconciled_then_two_distinct_merged_receipts_exist() {
    let root = tempfile::tempdir().unwrap();
    let (identity, run_dir) = identity_with_run(root.path(), "run-1", true);

    write_run_json_atomic(&run_dir.join("final.json"), &serde_json::json!({
        "version": 1,
        "runId": "run-1",
        "outcome": "merged",
        "finishedAt": "2026-08-11T10:00:30.000Z",
        "kind": "reflection",
        "generation": "2026-08-11T10:00:00.000Z",
    }), 0o600).unwrap();

    crate::worker::run_receipt_backfill::backfill_run_receipt_sync(&identity.paths.runtime, &run_dir, None, None);

    let (_, val2) = test_ledger(Some(serde_json::json!({ "startedAt": "2026-08-12T09:00:00.000Z" })));
    write_run_json_atomic(&run_dir.join("ledger.json"), &val2, 0o600).unwrap();
    write_run_json_atomic(&run_dir.join("final.json"), &serde_json::json!({
        "version": 1,
        "runId": "run-1",
        "outcome": "merged",
        "finishedAt": "2026-08-12T09:00:30.000Z",
        "kind": "reflection",
        "generation": "2026-08-12T09:00:00.000Z",
    }), 0o600).unwrap();

    crate::worker::run_receipt_backfill::backfill_run_receipt_sync(&identity.paths.runtime, &run_dir, None, None);

    let list = read_receipts(&identity.paths.runtime);
    let mut gens: Vec<_> = list.iter().filter(|r| r.input.event() == "merged").map(|r| match &r.input { MemoryReceiptInput::Run { generation, .. } => generation.as_str(), MemoryReceiptInput::Facts { .. } => panic!("expected run receipt") }).collect();
    gens.sort();
    assert_eq!(gens, vec!["2026-08-11T10:00:00.000Z", "2026-08-12T09:00:00.000Z"]);
}

#[test]
fn given_a_pre_ledger_abandoned_json_when_reconciled_twice_then_one_abandoned_receipt_keyed_by_sentinel_generation() {
    for kind in ["reflection", "dream"] {
        let root = tempfile::tempdir().unwrap();
        let (identity, run_dir) = identity_with_run(root.path(), "run-1", false);

        write_run_json_atomic(&run_dir.join("abandoned.json"), &serde_json::json!({
            "version": 1,
            "runId": "run-1",
            "kind": kind,
            "trigger": if kind == "dream" { "dream" } else { "step-count" },
            "generation": "2026-08-11T09:59:00.000Z",
            "reason": "launch_interrupted",
            "abandonedAt": "2026-08-11T10:00:05.000Z",
        }), 0o600).unwrap();

        crate::worker::run_receipt_backfill::backfill_run_receipt_sync(&identity.paths.runtime, &run_dir, None, None);
        crate::worker::run_receipt_backfill::backfill_run_receipt_sync(&identity.paths.runtime, &run_dir, None, None);

        let list = read_receipts(&identity.paths.runtime);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].input.kind(), kind);
        assert_eq!(list[0].input.event(), "abandoned");
        assert!(matches!(&list[0].input, MemoryReceiptInput::Run { generation, .. } if generation == "2026-08-11T09:59:00.000Z"));
        assert_eq!(list[0].input.reason(), Some("launch_interrupted"));
    }
}

#[test]
fn given_quarantined_json_beside_an_unreadable_ledger_when_reconciled_then_one_quarantined_receipt() {
    let root = tempfile::tempdir().unwrap();
    let (identity, run_dir) = identity_with_run(root.path(), "run-1", false);
    std::fs::write(run_dir.join("ledger.json"), "{ not json").unwrap();

    write_run_json_atomic(&run_dir.join("quarantined.json"), &serde_json::json!({
        "version": 1,
        "runId": "run-1",
        "kind": "dream",
        "trigger": "dream",
        "generation": "2026-08-11T09:58:00.000Z",
        "reason": "ledger_unreadable",
        "quarantinedAt": "2026-08-11T10:00:10.000Z",
    }), 0o600).unwrap();

    crate::worker::run_receipt_backfill::backfill_run_receipt_sync(&identity.paths.runtime, &run_dir, None, None);

    let list = read_receipts(&identity.paths.runtime);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].input.kind(), "dream");
    assert_eq!(list[0].input.event(), "quarantined");
    assert!(matches!(&list[0].input, MemoryReceiptInput::Run { generation, .. } if generation == "2026-08-11T09:58:00.000Z"));
    assert_eq!(list[0].input.reason(), Some("ledger_unreadable"));
}
