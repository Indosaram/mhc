use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use pretty_assertions::assert_eq;
use tempfile::tempdir;

use crate::identity::MemoryIdentityPaths;
use crate::identity::layout::build_identity_paths;
use crate::locks::{
    AcquireLockOptions, CreateLockRecordOptions, create_lock_record, facts_queue_lock_path,
    with_lock,
};

use crate::facts::failures_backoff::{FactsFailureFilter, FactsFailureTarget};
use crate::facts::failures_schema::FactsFailureReason;
use crate::facts::failures_store::{
    FactsFailureStore, FactsFailureStoreError, FactsFailureStoreOptions, RecordFailureRequest,
};
use crate::facts::schema::facts_queue_paths;

const IDENTITY: &str = "facts-store-agent";

fn identity_fixture() -> (tempfile::TempDir, MemoryIdentityPaths) {
    let dir = tempdir().expect("tempdir");
    let memory_root = dir.path().join("memory");
    let paths = build_identity_paths(&memory_root, IDENTITY);
    (dir, paths)
}

fn clock_from(start: i64) -> Arc<dyn Fn() -> i64 + Send + Sync> {
    let tick = AtomicI64::new(start);
    Arc::new(move || tick.fetch_add(1000, Ordering::SeqCst) + 1000)
}

fn target(
    conversation_id: &str,
    end_message_id: &str,
    end_snapshot_line: u64,
) -> FactsFailureTarget {
    FactsFailureTarget {
        conversation_id: conversation_id.to_string(),
        end_message_id: end_message_id.to_string(),
        end_snapshot_line,
    }
}

#[test]
fn test_given_an_identity_when_the_queue_layout_is_built_then_failures_json_sits_beside_consumed_json()
 {
    // given
    let (_dir, paths) = identity_fixture();

    // when
    let layout = facts_queue_paths(&paths);

    // then
    assert_eq!(
        layout.failures_path,
        paths.facts_queue.join("failures.json")
    );
}

#[test]
fn test_given_no_failures_file_when_the_store_reads_then_empty_state_is_returned() {
    // given
    let (_dir, paths) = identity_fixture();
    let store = FactsFailureStore::new(FactsFailureStoreOptions {
        identity_paths: paths,
        now: Some(clock_from(1_700_000_000_000)),
        lock_wait_ms: None,
    });

    // when
    let failures = store.read_failures().expect("read failures");

    // then
    assert_eq!(failures.version, 1);
    assert!(failures.entries.is_empty());
}

#[test]
fn test_given_a_recorded_failure_when_the_store_persists_it_then_the_file_is_0600_with_a_trailing_newline()
 {
    // given
    let (_dir, paths) = identity_fixture();
    let store = FactsFailureStore::new(FactsFailureStoreOptions {
        identity_paths: paths.clone(),
        now: Some(clock_from(1_700_000_000_000)),
        lock_wait_ms: None,
    });

    // when
    let written = store
        .record_failure(RecordFailureRequest {
            targets: vec![target("c1", "m2", 4)],
            failure_id: "fail-1".to_string(),
            reason: FactsFailureReason::ChildExit,
            detail: Some("503".to_string()),
        })
        .expect("record failure");

    // then
    assert_eq!(written.entries.len(), 1);
    let layout = facts_queue_paths(&paths);
    let raw = fs::read_to_string(&layout.failures_path).expect("read raw failures");
    assert!(raw.ends_with('\n'));

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mode = fs::metadata(&layout.failures_path)
            .expect("metadata")
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn test_given_a_persisted_failure_when_the_same_failure_id_replays_then_the_streak_stays_at_one() {
    // given
    let (_dir, paths) = identity_fixture();
    let store = FactsFailureStore::new(FactsFailureStoreOptions {
        identity_paths: paths,
        now: Some(clock_from(1_700_000_000_000)),
        lock_wait_ms: None,
    });
    store
        .record_failure(RecordFailureRequest {
            targets: vec![target("c1", "m2", 4)],
            failure_id: "fail-1".to_string(),
            reason: FactsFailureReason::ChildExit,
            detail: None,
        })
        .expect("first record");

    // when
    let replayed = store
        .record_failure(RecordFailureRequest {
            targets: vec![target("c1", "m2", 4)],
            failure_id: "fail-1".to_string(),
            reason: FactsFailureReason::ChildExit,
            detail: None,
        })
        .expect("replayed record");

    // then
    assert_eq!(replayed.entries.len(), 1);
    assert_eq!(replayed.entries[0].streak, 1);
}

#[test]
fn test_given_persisted_failures_when_the_batch_succeeds_then_only_its_records_are_cleared() {
    // given
    let (_dir, paths) = identity_fixture();
    let store = FactsFailureStore::new(FactsFailureStoreOptions {
        identity_paths: paths,
        now: Some(clock_from(1_700_000_000_000)),
        lock_wait_ms: None,
    });
    store
        .record_failure(RecordFailureRequest {
            targets: vec![target("c1", "m2", 4), target("c2", "m4", 8)],
            failure_id: "fail-1".to_string(),
            reason: FactsFailureReason::ChildExit,
            detail: None,
        })
        .expect("record two");

    // when
    let after_clear = store
        .clear_on_success(&[target("c1", "m2", 4)])
        .expect("clear c1");

    // then
    assert_eq!(after_clear.entries.len(), 1);
    assert_eq!(after_clear.entries[0].conversation_id, "c2");
}

#[test]
fn test_given_parked_records_when_a_retry_clears_one_conversation_then_the_rest_stay_parked() {
    // given
    let (_dir, paths) = identity_fixture();
    let store = FactsFailureStore::new(FactsFailureStoreOptions {
        identity_paths: paths,
        now: Some(clock_from(1_700_000_000_000)),
        lock_wait_ms: None,
    });
    store
        .record_failure(RecordFailureRequest {
            targets: vec![target("c1", "m2", 4), target("c2", "m4", 8)],
            failure_id: "fail-1".to_string(),
            reason: FactsFailureReason::ChildExit,
            detail: None,
        })
        .expect("record invalid");

    // when
    let removed = store
        .clear_for_retry(&FactsFailureFilter {
            conversation_id: Some("c1".to_string()),
            end_message_id: None,
        })
        .expect("clear for retry c1");

    // then
    assert_eq!(removed, 1);
    let remaining = store.read_failures().expect("read remaining");
    assert_eq!(remaining.entries.len(), 1);
    assert_eq!(remaining.entries[0].conversation_id, "c2");
}

#[test]
fn test_given_a_corrupt_failures_file_when_the_store_reads_then_it_fails_closed_with_a_typed_error()
{
    // given
    let (_dir, paths) = identity_fixture();
    let layout = facts_queue_paths(&paths);
    fs::create_dir_all(&layout.queue_dir).expect("create queue dir");
    fs::write(&layout.failures_path, "not json\n").expect("write corrupt");
    let store = FactsFailureStore::new(FactsFailureStoreOptions {
        identity_paths: paths,
        now: Some(clock_from(1_700_000_000_000)),
        lock_wait_ms: None,
    });

    // when
    let result = store.read_failures();

    // then
    assert!(matches!(result, Err(FactsFailureStoreError::Corrupt(_))));
}

#[test]
fn test_given_a_corrupt_failures_file_when_a_failure_is_recorded_then_the_write_fails_closed_and_the_file_is_untouched()
 {
    // given
    let (_dir, paths) = identity_fixture();
    let layout = facts_queue_paths(&paths);
    fs::create_dir_all(&layout.queue_dir).expect("create queue dir");
    fs::write(&layout.failures_path, "not json\n").expect("write corrupt");
    let store = FactsFailureStore::new(FactsFailureStoreOptions {
        identity_paths: paths,
        now: Some(clock_from(1_700_000_000_000)),
        lock_wait_ms: None,
    });

    // when
    let result = store.record_failure(RecordFailureRequest {
        targets: vec![target("c1", "m2", 4)],
        failure_id: "fail-1".to_string(),
        reason: FactsFailureReason::ChildExit,
        detail: None,
    });

    // then
    assert!(matches!(result, Err(FactsFailureStoreError::Corrupt(_))));
    assert_eq!(
        fs::read_to_string(&layout.failures_path).expect("read corrupt"),
        "not json\n"
    );
}

#[test]
fn test_given_the_facts_queue_lock_is_held_when_the_store_records_a_failure_then_it_waits_for_the_lock_instead_of_writing()
 {
    // given
    let (_dir, paths) = identity_fixture();
    let lock_path = facts_queue_lock_path(&paths.locks);
    fs::create_dir_all(lock_path.parent().unwrap()).expect("create locks dir");
    let record = create_lock_record("facts-queue", CreateLockRecordOptions::default()).unwrap();
    let options = AcquireLockOptions {
        wait_timeout_ms: Some(500),
        ..Default::default()
    };

    let store = FactsFailureStore::new(FactsFailureStoreOptions {
        identity_paths: paths,
        now: Some(clock_from(1_700_000_000_000)),
        lock_wait_ms: Some(50),
    });

    // when
    let lock_held_result = with_lock(&lock_path, &record, &options, || {
        let outcome = store.record_failure(RecordFailureRequest {
            targets: vec![target("c1", "m2", 4)],
            failure_id: "fail-1".to_string(),
            reason: FactsFailureReason::ChildExit,
            detail: None,
        });
        // then
        assert!(matches!(outcome, Err(FactsFailureStoreError::Lock(_))));
        Ok::<(), ()>(())
    });

    assert!(lock_held_result.is_ok());
}
