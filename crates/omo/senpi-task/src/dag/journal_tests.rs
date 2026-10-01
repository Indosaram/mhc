//! `dag/journal.test.ts`.

use std::sync::mpsc;
use std::time::Duration;

use pretty_assertions::assert_eq;

use super::*;
use crate::dag::store::{DagEventReadOptions, DagStoreConfig, DagStoreError, DagStoreOptions, create_dag_file_store};
use crate::dag::types::{DagNodeId, DagNodeState, DagNodeTransitionReason, DagRunEventPayload};

const RUN_ID: &str = "run-journal";

fn run_id() -> DagRunId {
    DagRunId::from(RUN_ID.to_string())
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct TestCheckpoint {
    #[serde(rename = "schemaVersion")]
    schema_version: u8,
    #[serde(rename = "runId")]
    run_id: DagRunId,
    #[serde(rename = "checkpointSeq")]
    checkpoint_seq: u64,
    generations: Vec<u64>,
}

impl DagJournalCheckpoint for TestCheckpoint {
    fn checkpoint_seq(&self) -> u64 {
        self.checkpoint_seq
    }
    fn with_checkpoint_seq(&self, checkpoint_seq: u64) -> Self {
        Self {
            checkpoint_seq,
            ..self.clone()
        }
    }
}

fn initial_checkpoint() -> TestCheckpoint {
    TestCheckpoint {
        schema_version: 1,
        run_id: run_id(),
        checkpoint_seq: 0,
        generations: Vec::new(),
    }
}

fn apply_event(checkpoint: &TestCheckpoint, event: &DagRunEvent) -> TestCheckpoint {
    match &event.payload {
        DagRunEventPayload::RunStarted { generation } => {
            let mut generations = checkpoint.generations.clone();
            generations.push(*generation);
            TestCheckpoint {
                generations,
                ..checkpoint.clone()
            }
        }
        _ => checkpoint.clone(),
    }
}

fn temp_store() -> Arc<DagFileStore> {
    // Leaked so the store's files outlive this test (no `afterEach` equivalent needed for a temp dir).
    let path = tempfile::tempdir().expect("tempdir").keep();
    Arc::new(
        create_dag_file_store(&DagStoreConfig::new(path), DagStoreOptions::default())
            .expect("store opens"),
    )
}

fn run_started(generation: u64) -> DagRunEventPayload {
    DagRunEventPayload::RunStarted { generation }
}

// Bounded wait replacing the TS `deferred()` promise pattern: fails loudly instead of hanging.
fn recv_within<T>(rx: &mpsc::Receiver<T>, what: &str) -> T {
    rx.recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| panic!("waited 5s for {what}, never fired"))
}

#[test]
fn given_three_mutations_when_appended_then_subscribers_see_durable_events_in_order_and_checkpoint_seq_reaches_three()
 {
    let store = temp_store();
    let journal = create_dag_journal(DagJournalOptions {
        store: Arc::clone(&store),
        run_id: run_id(),
        initial_checkpoint: initial_checkpoint(),
        apply_event: Arc::new(apply_event),
        subscriber_ring: None,
        now: None,
    })
    .expect("journal");
    let delivered: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let durable_at_delivery: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let store_for_listener = Arc::clone(&store);
    let delivered_for_listener = Arc::clone(&delivered);
    let durable_for_listener = Arc::clone(&durable_at_delivery);
    let _unsubscribe = journal.subscribe(Arc::new(move |event: &DagRunEvent| {
        delivered_for_listener.lock().unwrap().push(event.seq);
        let seq = store_for_listener
            .read_checkpoint::<TestCheckpoint>(&run_id())
            .ok()
            .flatten()
            .map(|checkpoint| checkpoint.checkpoint_seq as i64)
            .unwrap_or(-1);
        durable_for_listener.lock().unwrap().push(seq);
    }));

    journal.append(run_started(1)).expect("append 1");
    journal.append(run_started(2)).expect("append 2");
    journal.append(run_started(3)).expect("append 3");
    journal.when_idle();

    assert_eq!(*delivered.lock().unwrap(), vec![1, 2, 3]);
    // The pinned TS case asserts `durableAtDelivery == [3, 3, 3]` because JS runs the whole delivery
    // burst after the synchronous append burst (the microtask checkpoint at `await whenIdle()`).
    // The Rust port drains on a dedicated thread that may interleave with the next append, so the
    // faithful Rust invariant is durable-before-notify: every delivery observes its own commit (or
    // a later one) already on disk, and the burst still ends on the final checkpoint.
    let durable = durable_at_delivery.lock().unwrap().clone();
    assert_eq!(durable.len(), 3);
    for (index, seq) in durable.iter().enumerate() {
        assert!(
            *seq > index as i64,
            "event {} was delivered before its commit was durable: {durable:?}",
            index + 1
        );
    }
    assert_eq!(durable.last().copied(), Some(3));
    let checkpoint = store
        .read_checkpoint::<TestCheckpoint>(&run_id())
        .expect("read checkpoint")
        .expect("checkpoint present");
    assert_eq!(checkpoint.checkpoint_seq, 3);
    assert_eq!(checkpoint.generations, vec![1, 2, 3]);
}

#[test]
fn given_checkpoint_replacement_crashes_after_wal_append_when_reopened_on_the_same_store_then_replay_never_reaches_the_previous_journal_subscriber()
{
    use std::sync::atomic::{AtomicBool, Ordering};

    let store = temp_store();
    let fail_once = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&fail_once);
    store.set_checkpoint_hook(Arc::new(move |_value: &serde_json::Value| {
        if flag.swap(false, Ordering::SeqCst) {
            Err(DagStoreError::Message("injected checkpoint crash".to_string()))
        } else {
            Ok(())
        }
    }));

    let crashing = create_dag_journal(DagJournalOptions {
        store: Arc::clone(&store),
        run_id: run_id(),
        initial_checkpoint: initial_checkpoint(),
        apply_event: Arc::new(apply_event),
        subscriber_ring: None,
        now: None,
    })
    .expect("journal");
    let delivered_before_durability: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let collected_before = Arc::clone(&delivered_before_durability);
    let _unsubscribe_before = crashing.subscribe(Arc::new(move |event: &DagRunEvent| {
        collected_before.lock().unwrap().push(event.seq);
    }));

    // The checkpoint write fails after the WAL append, so the append reports the injected crash and
    // the subscriber is never notified of the un-durable event.
    let error = crashing.append(run_started(1)).expect_err("append crashes");
    assert_eq!(error.to_string(), "injected checkpoint crash");
    crashing.when_idle();

    // Reopened on the same store (the hook has spent its one failure): replay rebuilds seq 1 from
    // the WAL, so the next append lands on seq 2 and only that event reaches the new subscriber.
    let reopened = create_dag_journal(DagJournalOptions {
        store: Arc::clone(&store),
        run_id: run_id(),
        initial_checkpoint: initial_checkpoint(),
        apply_event: Arc::new(apply_event),
        subscriber_ring: None,
        now: None,
    })
    .expect("reopened journal");
    let delivered_after_reopen: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let collected_after = Arc::clone(&delivered_after_reopen);
    let _unsubscribe_after = reopened.subscribe(Arc::new(move |event: &DagRunEvent| {
        collected_after.lock().unwrap().push(event.seq);
    }));
    let second = reopened.append(run_started(2)).expect("append 2");
    reopened.when_idle();

    assert_eq!(*delivered_before_durability.lock().unwrap(), Vec::<u64>::new());
    assert_eq!(second.seq, 2);
    assert_eq!(*delivered_after_reopen.lock().unwrap(), vec![2]);
    let snapshot = reopened.snapshot();
    assert_eq!(snapshot.checkpoint_seq, 2);
    assert_eq!(snapshot.generations, vec![1, 2]);
    let events = store
        .read_events(
            &run_id(),
            0,
            &DagEventReadOptions {
                limit: 10,
                ..Default::default()
            },
        )
        .expect("read events")
        .events;
    assert_eq!(
        events.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[test]
fn after_wal_append_fault_leaves_reducer_unapplied_until_reopen() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let store = temp_store();
    let fail_once = AtomicBool::new(true);
    store.set_append_hook(Arc::new(move |_| {
        if fail_once.swap(false, Ordering::SeqCst) { Err(DagStoreError::Message("after WAL crash".into())) } else { Ok(()) }
    }));
    let options = || DagJournalOptions { store: Arc::clone(&store), run_id: run_id(), initial_checkpoint: initial_checkpoint(), apply_event: Arc::new(apply_event), subscriber_ring: None, now: None };
    let first = create_dag_journal(options()).unwrap();
    assert_eq!(first.append(run_started(1)).unwrap_err().to_string(), "after WAL crash");
    assert_eq!(first.snapshot().generations, Vec::<u64>::new());
    let reopened = create_dag_journal(options()).unwrap();
    assert_eq!(reopened.snapshot().generations, vec![1]);
    assert_eq!(reopened.snapshot().checkpoint_seq, 1);
}

#[test]
fn given_a_queued_node_transition_when_appended_then_the_journal_preserves_its_queue_position() {
    let store = temp_store();
    let journal = create_dag_journal(DagJournalOptions {
        store: Arc::clone(&store),
        run_id: run_id(),
        initial_checkpoint: initial_checkpoint(),
        apply_event: Arc::new(apply_event),
        subscriber_ring: None,
        now: None,
    })
    .expect("journal");
    let node_id: DagNodeId = "node-a".to_string();

    journal
        .append(DagRunEventPayload::NodeTransitioned {
            node_id: node_id.clone(),
            from: DagNodeState::Scheduled,
            to: DagNodeState::Scheduled,
            reason: DagNodeTransitionReason::TaskQueued { queue_position: 3 },
        })
        .expect("append transition");

    let events = store
        .read_events(&run_id(), 0, &DagEventReadOptions { limit: 10, ..Default::default() })
        .expect("read events")
        .events;
    let found = events.iter().any(|event| match &event.payload {
        DagRunEventPayload::NodeTransitioned {
            node_id: found_node,
            reason: DagNodeTransitionReason::TaskQueued { queue_position },
            ..
        } => found_node == &node_id && *queue_position == 3,
        _ => false,
    });
    assert!(found, "expected a queued node-transitioned event with queuePosition 3");
}

#[test]
fn given_a_durable_checkpoint_and_wal_when_reopened_then_sequence_numbers_continue_from_the_greater_persisted_tail()
 {
    let store = temp_store();
    let first = create_dag_journal(DagJournalOptions {
        store: Arc::clone(&store),
        run_id: run_id(),
        initial_checkpoint: initial_checkpoint(),
        apply_event: Arc::new(apply_event),
        subscriber_ring: None,
        now: None,
    })
    .expect("first journal");
    first.append(run_started(1)).expect("append 1");
    first.append(run_started(2)).expect("append 2");

    let reopened = create_dag_journal(DagJournalOptions {
        store: Arc::clone(&store),
        run_id: run_id(),
        initial_checkpoint: initial_checkpoint(),
        apply_event: Arc::new(apply_event),
        subscriber_ring: None,
        now: None,
    })
    .expect("reopened journal");
    let event = reopened.append(run_started(3)).expect("append 3");

    assert_eq!(event.seq, 3);
    assert_eq!(reopened.snapshot().checkpoint_seq, 3);
}

#[test]
fn given_a_durable_commit_subscription_when_it_is_removed_before_another_journal_commits_then_no_phantom_delivery_remains()
 {
    let store = temp_store();
    let first = create_dag_journal(DagJournalOptions {
        store: Arc::clone(&store),
        run_id: run_id(),
        initial_checkpoint: initial_checkpoint(),
        apply_event: Arc::new(apply_event),
        subscriber_ring: None,
        now: None,
    })
    .expect("first journal");
    let reopened = create_dag_journal(DagJournalOptions {
        store: Arc::clone(&store),
        run_id: run_id(),
        initial_checkpoint: initial_checkpoint(),
        apply_event: Arc::new(apply_event),
        subscriber_ring: None,
        now: None,
    })
    .expect("reopened journal");
    let delivered: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let delivered_for_listener = Arc::clone(&delivered);
    let unsubscribe = subscribe_dag_journal(
        &store,
        &run_id(),
        Arc::new(move |event: &DagRunEvent| {
            delivered_for_listener.lock().unwrap().push(event.seq);
        }),
    );

    unsubscribe();
    reopened.append(run_started(1)).expect("append 1");
    reopened.when_idle();

    assert_eq!(*delivered.lock().unwrap(), Vec::<u64>::new());
    assert_eq!(first.snapshot().checkpoint_seq, 0);
}

#[test]
fn given_a_subscriber_blocked_on_its_first_event_when_its_ring_overflows_then_it_receives_one_coalesced_overflow_with_the_last_delivered_recovery_seq()
 {
    let store = temp_store();
    let journal = create_dag_journal(DagJournalOptions {
        store: Arc::clone(&store),
        run_id: run_id(),
        initial_checkpoint: initial_checkpoint(),
        apply_event: Arc::new(apply_event),
        subscriber_ring: Some(2),
        now: None,
    })
    .expect("journal");
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (first_started_tx, first_started_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let delivered: Arc<Mutex<Vec<DagRunEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let delivered_for_listener = Arc::clone(&delivered);
    let _unsubscribe = journal.subscribe(Arc::new(move |event: &DagRunEvent| {
        delivered_for_listener.lock().unwrap().push(event.clone());
        if event.seq == 1 && matches!(event.payload, DagRunEventPayload::RunStarted { .. }) {
            let _ = first_started_tx.send(());
            let _ = release_rx.lock().unwrap().recv_timeout(Duration::from_secs(5));
        }
    }));
    journal.append(run_started(1)).expect("append 1");
    recv_within(&first_started_rx, "first_started");

    journal.append(run_started(2)).expect("append 2");
    journal.append(run_started(3)).expect("append 3");
    journal.append(run_started(4)).expect("append 4");
    journal.append(run_started(5)).expect("append 5");
    let _ = release_tx.send(());
    journal.when_idle();

    let delivered = delivered.lock().unwrap();
    let kinds: Vec<bool> = delivered
        .iter()
        .map(|event| matches!(event.payload, DagRunEventPayload::RunStarted { .. }))
        .collect();
    assert_eq!(kinds, vec![true, false, true, true]);
    let DagRunEventPayload::StreamOverflow {
        dropped_count,
        recover_after_seq,
    } = &delivered[1].payload
    else {
        panic!("expected overflow at index 1");
    };
    assert_eq!(*dropped_count, 2);
    assert_eq!(*recover_after_seq, 1);
    assert_eq!(
        delivered[2..].iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![4, 5]
    );
}

#[test]
fn given_a_viewer_recovering_a_subscriber_overflow_when_it_catches_up_from_the_recovery_cursor_then_every_wal_seq_is_applied_once_without_gaps()
 {
    let store = temp_store();
    let journal = create_dag_journal(DagJournalOptions {
        store: Arc::clone(&store),
        run_id: run_id(),
        initial_checkpoint: initial_checkpoint(),
        apply_event: Arc::new(apply_event),
        subscriber_ring: Some(2),
        now: None,
    })
    .expect("journal");
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (first_started_tx, first_started_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let applied: Arc<Mutex<std::collections::BTreeMap<u64, DagRunEvent>>> =
        Arc::new(Mutex::new(std::collections::BTreeMap::new()));
    let store_for_listener = Arc::clone(&store);
    let applied_for_listener = Arc::clone(&applied);
    let _unsubscribe = journal.subscribe(Arc::new(move |event: &DagRunEvent| {
        let apply_once = |event: &DagRunEvent, applied: &Arc<Mutex<std::collections::BTreeMap<u64, DagRunEvent>>>| {
            let mut applied = applied.lock().unwrap();
            applied.entry(event.seq).or_insert_with(|| event.clone());
        };
        if let DagRunEventPayload::StreamOverflow { recover_after_seq, .. } = &event.payload {
            let recovery = store_for_listener
                .read_events(
                    &run_id(),
                    *recover_after_seq,
                    &DagEventReadOptions {
                        limit: 100,
                        through_seq: Some(event.seq),
                        ..Default::default()
                    },
                )
                .expect("recovery read");
            for recovered in &recovery.events {
                apply_once(recovered, &applied_for_listener);
            }
            return;
        }
        apply_once(event, &applied_for_listener);
        if event.seq == 1 {
            let _ = first_started_tx.send(());
            let _ = release_rx.lock().unwrap().recv_timeout(Duration::from_secs(5));
        }
    }));
    journal.append(run_started(1)).expect("append 1");
    recv_within(&first_started_rx, "first_started");

    journal.append(run_started(2)).expect("append 2");
    journal.append(run_started(3)).expect("append 3");
    journal.append(run_started(4)).expect("append 4");
    journal.append(run_started(5)).expect("append 5");
    let _ = release_tx.send(());
    journal.when_idle();

    let wal = store
        .read_events(&run_id(), 0, &DagEventReadOptions { limit: 100, ..Default::default() })
        .expect("read wal")
        .events;
    let seqs: Vec<u64> = wal.iter().map(|event| event.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4, 5, 6]);
    let unique: std::collections::BTreeSet<u64> = seqs.iter().copied().collect();
    assert_eq!(unique.len(), seqs.len());
    let last = wal.last().expect("wal not empty");
    assert_eq!(last.seq, 6);
    let DagRunEventPayload::StreamOverflow { dropped_count, recover_after_seq } = &last.payload else {
        panic!("expected trailing overflow event");
    };
    assert_eq!(*dropped_count, 2);
    assert_eq!(*recover_after_seq, 1);
    let applied = applied.lock().unwrap();
    let keys: Vec<u64> = applied.keys().copied().collect();
    assert_eq!(keys, vec![1, 2, 3, 4, 5, 6]);
}

#[test]
fn given_a_slow_async_listener_when_another_mutation_is_appended_then_the_mutation_completes_before_the_listener_catches_up()
 {
    let store = temp_store();
    let journal = create_dag_journal(DagJournalOptions {
        store: Arc::clone(&store),
        run_id: run_id(),
        initial_checkpoint: initial_checkpoint(),
        apply_event: Arc::new(apply_event),
        subscriber_ring: None,
        now: None,
    })
    .expect("journal");
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (listener_started_tx, listener_started_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let listener_started_tx = Arc::new(Mutex::new(Some(listener_started_tx)));
    let _unsubscribe = journal.subscribe(Arc::new(move |_event: &DagRunEvent| {
        if let Some(tx) = listener_started_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
        let _ = release_rx.lock().unwrap().recv_timeout(Duration::from_secs(5));
    }));
    journal.append(run_started(1)).expect("append 1");
    recv_within(&listener_started_rx, "listener_started");

    let second = journal.append(run_started(2)).expect("append 2");

    assert_eq!(second.seq, 2);
    assert_eq!(journal.snapshot().checkpoint_seq, 2);
    let _ = release_tx.send(());
    journal.when_idle();
}

#[test]
fn given_an_active_subscription_when_it_is_removed_then_later_events_are_not_delivered() {
    let store = temp_store();
    let journal = create_dag_journal(DagJournalOptions {
        store: Arc::clone(&store),
        run_id: run_id(),
        initial_checkpoint: initial_checkpoint(),
        apply_event: Arc::new(apply_event),
        subscriber_ring: None,
        now: None,
    })
    .expect("journal");
    let delivered: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let delivered_for_listener = Arc::clone(&delivered);
    let unsubscribe = journal.subscribe(Arc::new(move |event: &DagRunEvent| {
        delivered_for_listener.lock().unwrap().push(event.seq);
    }));
    journal.append(run_started(1)).expect("append 1");
    journal.when_idle();

    unsubscribe();
    journal.append(run_started(2)).expect("append 2");
    journal.when_idle();

    assert_eq!(*delivered.lock().unwrap(), vec![1]);
}
