use std::path::Path;
use memory_core::{
    facts::failures_store::{FactsFailureStore, FactsFailureStoreOptions},
    identity::layout::build_identity_paths,
    receipts::{read_memory_receipts, MemoryReceipt, MemoryReceiptInput, ReadMemoryReceiptsOptions},
};
use crate::{
    facts_runner_types::FactsRunLedger,
    facts_terminal_writes::{FactsFailureWrite, FactsTerminalWrites},
    receipts_port::MemoryReceiptsPort,
};

const NOW: &str = "2026-08-11T10:00:00.000Z";

fn facts_run(root: &Path) -> (memory_core::identity::layout::MemoryIdentityPaths, std::path::PathBuf, String) {
    let paths = build_identity_paths(root, "agent-test");
    let batch_id = "batch-0001".to_string();
    let run_dir = paths.facts.join("runs").join("facts-abc-1");
    std::fs::create_dir_all(&run_dir).unwrap();
    let ledger = serde_json::json!({
        "version": 1,
        "runId": "facts-abc-1",
        "kind": "facts",
        "startedAt": NOW,
        "hardDeadlineAt": 1,
        "terminationGraceMs": 1,
        "deadlineAt": 2,
        "batchId": batch_id,
        "queued": [],
    });
    crate::worker::run_artifacts::write_run_json_atomic(&run_dir.join("ledger.json"), &ledger, 0o600).unwrap();
    (paths, run_dir, batch_id)
}

struct TestIo<'a> {
    runtime_dir: &'a Path,
    port: Option<&'a dyn MemoryReceiptsPort>,
}

impl crate::facts_terminal_writes::FactsTerminalIo for TestIo<'_> {
    fn now(&self) -> String {
        NOW.to_string()
    }
    fn mark_consumed(&mut self, _entries: &[memory_core::facts::FactsQueueEntry]) -> std::io::Result<()> {
        Ok(())
    }
    fn warn(&mut self, _message: &str, _detail: &str) {}
    fn receipts_dir(&self) -> Option<&Path> {
        Some(self.runtime_dir)
    }
    fn receipts_port(&self) -> Option<&dyn MemoryReceiptsPort> {
        self.port
    }
}

fn read_receipts(runtime_dir: &Path) -> Vec<MemoryReceipt> {
    let mut list = read_memory_receipts(runtime_dir, &ReadMemoryReceiptsOptions::default()).unwrap().receipts;
    list.reverse();
    list
}

#[test]
fn given_a_committed_batch_when_it_succeeds_then_one_committed_receipt_carries_the_batch_id_and_sha() {
    let root = tempfile::tempdir().unwrap();
    let (paths, run_dir, batch_id) = facts_run(root.path());
    let store = FactsFailureStore::new(FactsFailureStoreOptions {
        identity_paths: paths.clone(),
        now: None,
        lock_wait_ms: None,
    });
    let mut io = TestIo { runtime_dir: &paths.runtime, port: None };
    let mut writes = FactsTerminalWrites { failures: &store, io: &mut io };

    writes.succeed(&run_dir, "facts-abc-1", crate::facts_runner_types::FactsTerminalOutcome::Committed, &[], &[], Some("f00d")).unwrap();

    let list = read_receipts(&paths.runtime);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].input.kind(), "facts");
    assert!(matches!(&list[0].input, MemoryReceiptInput::Facts { batch_id: actual, .. } if actual == &batch_id));
    assert_eq!(list[0].input.event(), "committed");
    assert_eq!(list[0].input.sha(), Some("f00d"));
}

#[test]
fn given_an_empty_extraction_when_it_succeeds_then_the_receipt_says_no_facts() {
    let root = tempfile::tempdir().unwrap();
    let (paths, run_dir, _batch_id) = facts_run(root.path());
    let store = FactsFailureStore::new(FactsFailureStoreOptions {
        identity_paths: paths.clone(),
        now: None,
        lock_wait_ms: None,
    });
    let mut io = TestIo { runtime_dir: &paths.runtime, port: None };
    let mut writes = FactsTerminalWrites { failures: &store, io: &mut io };

    writes.succeed(&run_dir, "facts-abc-1", crate::facts_runner_types::FactsTerminalOutcome::NoFacts, &[], &[], None).unwrap();

    let list = read_receipts(&paths.runtime);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].input.event(), "no_facts");
}

#[test]
fn given_a_failure_that_parks_its_endpoint_when_recorded_then_failed_and_parked_receipts_follow_the_sentinel() {
    let root = tempfile::tempdir().unwrap();
    let (paths, run_dir, batch_id) = facts_run(root.path());
    let store = FactsFailureStore::new(FactsFailureStoreOptions {
        identity_paths: paths.clone(),
        now: None,
        lock_wait_ms: None,
    });

    struct RecordingPort<'a> {
        run_dir: &'a Path,
        order: std::sync::Mutex<Vec<(String, bool)>>,
    }
    impl MemoryReceiptsPort for RecordingPort<'_> {
        fn append(&self, runtime_dir: &Path, input: &MemoryReceiptInput) -> Result<bool, String> {
            self.order.lock().unwrap().push((input.event().to_string(), self.run_dir.join("final.json").exists()));
            memory_core::receipts::append_memory_receipt_once(runtime_dir, input).map_err(|e| e.to_string())
        }
    }

    let recording = RecordingPort { run_dir: &run_dir, order: std::sync::Mutex::new(Vec::new()) };
    let mut io = TestIo { runtime_dir: &paths.runtime, port: Some(&recording) };
    let mut writes = FactsTerminalWrites { failures: &store, io: &mut io };

    let target = memory_core::facts::failures_backoff::FactsFailureTarget {
        conversation_id: "conversation-a".into(),
        end_message_id: "m-9".into(),
        end_snapshot_line: 9,
    };

    // Parked failure requires multiple failures in backoff
    for attempt in 0..5 {
        let _ = writes.fail(&FactsFailureWrite {
            run_dir: &run_dir,
            run_id: "facts-abc-1",
            batch_id: &batch_id,
            targets: std::slice::from_ref(&target),
            reason: memory_core::facts::FactsFailureReason::PayloadEntryOversize,
            detail: "refused",
            outcome: None,
        });
        if attempt < 4 {
            let _ = std::fs::remove_file(run_dir.join("final.json"));
        }
    }

    let recorded = recording.order.lock().unwrap().clone();
    assert!(recorded.iter().any(|(ev, sentinel)| ev == "failed" && *sentinel));
    assert!(recorded.iter().any(|(ev, sentinel)| ev == "parked" && *sentinel));
    let list = read_receipts(&paths.runtime);
    assert!(list.iter().any(|r| matches!(&r.input, MemoryReceiptInput::Facts { batch_id: id, .. } if id == &batch_id) && r.input.event() == "failed"));
    assert!(list.iter().any(|r| matches!(&r.input, MemoryReceiptInput::Facts { batch_id: id, .. } if id == &batch_id) && r.input.event() == "parked"));
}

#[test]
fn given_a_failure_that_only_backs_off_when_recorded_then_failed_is_receipted_and_parked_is_not() {
    let root = tempfile::tempdir().unwrap();
    let (paths, run_dir, batch_id) = facts_run(root.path());
    let store = FactsFailureStore::new(FactsFailureStoreOptions {
        identity_paths: paths.clone(),
        now: None,
        lock_wait_ms: None,
    });
    let mut io = TestIo { runtime_dir: &paths.runtime, port: None };
    let mut writes = FactsTerminalWrites { failures: &store, io: &mut io };

    let target = memory_core::facts::failures_backoff::FactsFailureTarget {
        conversation_id: "conversation-a".into(),
        end_message_id: "m-9".into(),
        end_snapshot_line: 9,
    };

    writes.fail(&FactsFailureWrite {
        run_dir: &run_dir,
        run_id: "facts-abc-1",
        batch_id: &batch_id,
        targets: &[target],
        reason: memory_core::facts::FactsFailureReason::ChildExit,
        detail: "exit 1",
        outcome: None,
    }).unwrap();

    let list = read_receipts(&paths.runtime);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].input.event(), "failed");
}

#[test]
fn given_a_run_reconciliation_cannot_prove_dead_when_abandoned_then_an_abandoned_receipt_follows_abandoned_json() {
    let root = tempfile::tempdir().unwrap();
    let (paths, run_dir, batch_id) = facts_run(root.path());
    let store = FactsFailureStore::new(FactsFailureStoreOptions {
        identity_paths: paths.clone(),
        now: None,
        lock_wait_ms: None,
    });
    let mut io = TestIo { runtime_dir: &paths.runtime, port: None };
    let mut writes = FactsTerminalWrites { failures: &store, io: &mut io };

    let ledger = FactsRunLedger {
        version: 1,
        run_id: "facts-abc-1".into(),
        attempt: None,
        model: None,
        thinking: None,
        kind: "facts".into(),
        started_at: NOW.into(),
        hard_deadline_at: 1.0,
        termination_grace_ms: 1.0,
        deadline_at: 2.0,
        batch_id: batch_id.clone(),
        queued: vec![],
        head_before_apply: None,
        apply_recovery: None,
        pid: None,
        process_start: None,
        child_pid: None,
        child_process_start: None,
    };

    writes.abandon(&run_dir, &ledger).unwrap();

    assert!(run_dir.join("abandoned.json").exists());
    let list = read_receipts(&paths.runtime);
    assert_eq!(list.len(), 1);
    assert!(matches!(&list[0].input, MemoryReceiptInput::Facts { batch_id: id, .. } if id == &batch_id));
    assert_eq!(list[0].input.event(), "abandoned");
    assert_eq!(list[0].input.reason(), Some("unknown_liveness"));
}

#[test]
fn given_a_receipt_write_that_fails_when_the_batch_succeeds_then_the_run_still_finishes_and_final_json_lands() {
    let root = tempfile::tempdir().unwrap();
    let (paths, run_dir, _batch_id) = facts_run(root.path());
    let store = FactsFailureStore::new(FactsFailureStoreOptions {
        identity_paths: paths.clone(),
        now: None,
        lock_wait_ms: None,
    });

    struct FailingPort;
    impl MemoryReceiptsPort for FailingPort {
        fn append(&self, _runtime_dir: &Path, _input: &MemoryReceiptInput) -> Result<bool, String> {
            Err("disk full".into())
        }
    }

    let failing = FailingPort;
    let mut io = TestIo { runtime_dir: &paths.runtime, port: Some(&failing) };
    let mut writes = FactsTerminalWrites { failures: &store, io: &mut io };

    writes.succeed(&run_dir, "facts-abc-1", crate::facts_runner_types::FactsTerminalOutcome::Committed, &[], &[], Some("f00d")).unwrap();

    assert!(run_dir.join("final.json").exists());
    let list = read_receipts(&paths.runtime);
    assert_eq!(list.len(), 0);
}

#[test]
fn given_a_facts_batch_reserved_for_launch_when_its_run_dir_is_claimed_then_a_launched_receipt_carries_its_batch_id() {
    let root = tempfile::tempdir().unwrap();
    let paths = build_identity_paths(root.path(), "agent-test");

    let run_dir = crate::facts_run_storage::reserve_facts_run_dir(&crate::facts_run_storage::ReserveFactsRunDirOptions {
        facts_dir: &paths.facts,
        locks_dir: &paths.locks,
        entries: &[],
        batch_id: "batch-launch",
        launched_at: 0,
        deadline_ms: None,
        termination_grace_ms: None,
        lock_wait_ms: None,
        receipts_dir: Some(&paths.runtime),
        receipts: None,
        warn: None,
    }).unwrap();

    assert!(run_dir.is_some());
    let list = read_receipts(&paths.runtime);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].input.kind(), "facts");
    assert!(matches!(&list[0].input, MemoryReceiptInput::Facts { batch_id, .. } if batch_id == "batch-launch"));
    assert_eq!(list[0].input.event(), "launched");
}

#[test]
fn given_a_committed_run_whose_receipt_write_was_lost_when_facts_reconcile_twice_then_exactly_one_committed_receipt_is_rebuilt() {
    let root = tempfile::tempdir().unwrap();
    let (paths, run_dir, batch_id) = facts_run(root.path());
    let store = FactsFailureStore::new(FactsFailureStoreOptions {
        identity_paths: paths.clone(),
        now: None,
        lock_wait_ms: None,
    });

    struct FailingPort;
    impl MemoryReceiptsPort for FailingPort {
        fn append(&self, _runtime_dir: &Path, _input: &MemoryReceiptInput) -> Result<bool, String> {
            Err("receipt write lost".into())
        }
    }

    let failing = FailingPort;
    let mut io = TestIo { runtime_dir: &paths.runtime, port: Some(&failing) };
    let mut writes = FactsTerminalWrites { failures: &store, io: &mut io };
    writes.succeed(&run_dir, "facts-abc-1", crate::facts_runner_types::FactsTerminalOutcome::Committed, &[], &[], Some("f00d")).unwrap();
    assert_eq!(read_receipts(&paths.runtime).len(), 0);

    struct ReconcilePort<'a> {
        runtime_dir: &'a Path,
    }
    impl crate::facts_run_reconcile::FactsReconciliationPort for ReconcilePort<'_> {
        type Error = String;
        fn now_ms(&self) -> f64 { 0.0 }
        fn finalize(&mut self, _dir: &Path) -> Result<(), Self::Error> { panic!("never") }
        fn fail(&mut self, _dir: &Path, _ledger: &FactsRunLedger, _detail: &str) -> Result<(), Self::Error> { panic!("never") }
        fn abandon(&mut self, _dir: &Path, _ledger: &FactsRunLedger) -> Result<(), Self::Error> { panic!("never") }
        fn warn(&mut self, _run_id: &str, _error: &Self::Error) {}
        fn receipts_dir(&self) -> Option<&Path> { Some(self.runtime_dir) }
    }

    let mut port = ReconcilePort { runtime_dir: &paths.runtime };
    crate::facts_run_reconcile::reconcile_facts_runs(&paths.facts, &mut port).unwrap();
    crate::facts_run_reconcile::reconcile_facts_runs(&paths.facts, &mut port).unwrap();

    let list = read_receipts(&paths.runtime);
    assert_eq!(list.len(), 1);
    assert!(matches!(&list[0].input, MemoryReceiptInput::Facts { batch_id: id, .. } if id == &batch_id));
    assert_eq!(list[0].input.event(), "committed");
    assert_eq!(list[0].input.sha(), Some("f00d"));
}

#[test]
fn given_a_terminal_run_whose_receipt_was_already_written_when_facts_reconcile_then_no_second_receipt_appears() {
    let root = tempfile::tempdir().unwrap();
    let (paths, run_dir, _batch_id) = facts_run(root.path());
    let store = FactsFailureStore::new(FactsFailureStoreOptions {
        identity_paths: paths.clone(),
        now: None,
        lock_wait_ms: None,
    });
    let mut io = TestIo { runtime_dir: &paths.runtime, port: None };
    let mut writes = FactsTerminalWrites { failures: &store, io: &mut io };
    writes.succeed(&run_dir, "facts-abc-1", crate::facts_runner_types::FactsTerminalOutcome::NoFacts, &[], &[], None).unwrap();

    struct ReconcilePort<'a> {
        runtime_dir: &'a Path,
    }
    impl crate::facts_run_reconcile::FactsReconciliationPort for ReconcilePort<'_> {
        type Error = String;
        fn now_ms(&self) -> f64 { 0.0 }
        fn finalize(&mut self, _dir: &Path) -> Result<(), Self::Error> { panic!("never") }
        fn fail(&mut self, _dir: &Path, _ledger: &FactsRunLedger, _detail: &str) -> Result<(), Self::Error> { panic!("never") }
        fn abandon(&mut self, _dir: &Path, _ledger: &FactsRunLedger) -> Result<(), Self::Error> { panic!("never") }
        fn warn(&mut self, _run_id: &str, _error: &Self::Error) {}
        fn receipts_dir(&self) -> Option<&Path> { Some(self.runtime_dir) }
    }

    let mut port = ReconcilePort { runtime_dir: &paths.runtime };
    crate::facts_run_reconcile::reconcile_facts_runs(&paths.facts, &mut port).unwrap();

    let list = read_receipts(&paths.runtime);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].input.event(), "no_facts");
}

#[test]
fn given_a_launched_receipt_write_that_fails_when_a_facts_run_dir_is_reserved_then_the_failure_is_reported_to_warn_and_the_run_dir_is_still_claimed() {
    let root = tempfile::tempdir().unwrap();
    let paths = build_identity_paths(root.path(), "agent-test");
    let warnings = std::sync::Mutex::new(Vec::new());

    struct FailingPort;
    impl MemoryReceiptsPort for FailingPort {
        fn append(&self, _runtime_dir: &Path, _input: &MemoryReceiptInput) -> Result<bool, String> {
            Err("disk full".into())
        }
    }

    let failing = FailingPort;
    let warn_fn = |msg: &str| warnings.lock().unwrap().push(msg.to_string());

    let run_dir = crate::facts_run_storage::reserve_facts_run_dir(&crate::facts_run_storage::ReserveFactsRunDirOptions {
        facts_dir: &paths.facts,
        locks_dir: &paths.locks,
        entries: &[],
        batch_id: "batch-0002",
        launched_at: 0,
        deadline_ms: None,
        termination_grace_ms: None,
        lock_wait_ms: None,
        receipts_dir: Some(&paths.runtime),
        receipts: Some(&failing),
        warn: Some(&warn_fn),
    }).unwrap();

    assert!(run_dir.is_some());
    assert_eq!(warnings.lock().unwrap().len(), 1);
}
