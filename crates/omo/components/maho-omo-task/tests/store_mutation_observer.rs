use maho_omo_task::store_mutation_observer::{
    MutationNotifyingStore, create_mutation_notifying_store,
};
use senpi_task::{
    state::{TaskRecord, TaskRecordInput, TaskTransition, create_task_record},
    store::{PersistedTaskEvent, StateDirConfig, TaskRecordStore, TombstoneResult},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
fn fixture() -> (
    tempfile::TempDir,
    MutationNotifyingStore,
    Arc<AtomicUsize>,
    TaskRecord,
) {
    let root = tempfile::tempdir().expect("root");
    let store = TaskRecordStore::new(&StateDirConfig {
        project_dir: root.path().into(),
        task_state_dir: None,
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let observed = create_mutation_notifying_store(
        store,
        Arc::new(move |_change| {
            count.fetch_add(1, Ordering::SeqCst);
        }),
    );
    let record = create_task_record(TaskRecordInput::default(), Some(1)).expect("record");
    (root, observed, calls, record)
}
#[test]
fn save_and_replace_notify_after_persistence() {
    let (_root, store, calls, record) = fixture();
    store.save(&record).expect("save");
    store.replace(&record).expect("replace");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(store.load(&record.task_id).expect("load"), Some(record));
}
#[test]
fn failed_save_does_not_notify() {
    let (_root, store, calls, record) = fixture();
    store.save(&record).expect("save");
    assert!(store.save(&record).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
#[test]
fn reads_and_event_appends_do_not_notify() {
    let (_root, store, calls, record) = fixture();
    store.save(&record).expect("save");
    store.load(&record.task_id).expect("load");
    store.list().expect("list");
    store.list_expunging().expect("expunging");
    store
        .append_event(
            &record.task_id,
            &PersistedTaskEvent {
                event_type: "event".into(),
                payload: serde_json::json!({}),
            },
        )
        .expect("event");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
#[test]
fn unchanged_mutation_still_notifies() {
    let (_root, store, calls, record) = fixture();
    store.save(&record).expect("save");
    store.mutate(&record.task_id, Clone::clone).expect("mutate");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
#[test]
fn remove_notifies_even_when_already_missing() {
    let (_root, store, calls, record) = fixture();
    store.list().expect("initialize task directory");
    store.remove(&record.task_id).expect("remove");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
#[test]
fn retained_tombstone_does_not_notify() {
    let (_root, store, calls, record) = fixture();
    store.save(&record).expect("save");
    assert!(matches!(
        store
            .tombstone_if_expired(&record.task_id, |_| true)
            .expect("tombstone"),
        TombstoneResult::Retained
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
#[test]
fn committed_tombstone_and_expunge_both_notify() {
    let (_root, store, calls, record) = fixture();
    store.save(&record).expect("save");
    assert!(matches!(
        store
            .tombstone_if_expired(&record.task_id, |_| false)
            .expect("tombstone"),
        TombstoneResult::Tombstoned(_)
    ));
    store.complete_expunge(&record.task_id).expect("expunge");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}
#[test]
fn missing_tombstone_does_not_notify() {
    let (_root, store, calls, record) = fixture();
    assert!(matches!(
        store
            .tombstone_if_expired(&record.task_id, |_| false)
            .expect("tombstone"),
        TombstoneResult::Missing
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
#[test]
fn transition_notifies_after_record_is_committed() {
    let (_root, store, calls, record) = fixture();
    store.save(&record).expect("save");
    let result = store
        .transition(
            &record.task_id,
            &TaskTransition::Start {
                timestamp: "1970-01-01T00:00:00.002Z".into(),
                pid: None,
                child_session_id: None,
            },
        )
        .expect("transition");
    assert!(result.applied);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        store.load(&record.task_id).expect("load"),
        Some(result.record)
    );
}

#[test]
fn payload_identifies_saved_removed_and_restored_records() {
    let root = tempfile::tempdir().unwrap();
    let backing = TaskRecordStore::new(&StateDirConfig {
        project_dir: root.path().into(),
        task_state_dir: None,
    });
    let changes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let capture = changes.clone();
    let store = create_mutation_notifying_store(
        backing,
        Arc::new(move |change| capture.lock().unwrap().push(change)),
    );
    let record = create_task_record(TaskRecordInput::default(), Some(1)).unwrap();
    store.save(&record).unwrap();
    store
        .tombstone_if_expired(&record.task_id, |_| false)
        .unwrap();
    store.restore_expunging(&record.task_id).unwrap();
    store.remove(&record.task_id).unwrap();
    let changes = changes.lock().unwrap();
    assert_eq!(changes.len(), 4);
    assert!(
        changes
            .iter()
            .all(|change| change.task_id == record.task_id)
    );
    assert_eq!(changes[0].record, Some(record.clone()));
    assert!(changes[1].record.is_none());
    assert_eq!(changes[2].record, Some(record));
    assert!(changes[3].record.is_none());
}
