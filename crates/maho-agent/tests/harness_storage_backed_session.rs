//! Port of senpi packages/agent/test/harness/storage-backed-session.test.ts.

mod support;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use maho_agent::harness::context::BACKGROUND_CONTEXT;
use maho_agent::harness::session::commit::{insert_entry, insert_usage};
use maho_agent::harness::session::session::{SessionErrorKind, session_invariant_error};
use maho_agent::harness::session::testing::{GatingStorage, InstrumentedStorage};
use maho_agent::harness::session::types::{
    EntryKind, EntryScan, NewEntry, NewUsageRow, Session, SessionMetadata, SessionReader, Storage, Write,
};
use maho_agent::harness::session::values::{append_list, list, session_name, set_value, value};
use maho_agent::harness::session::{MemoryStorage, MemoryStorageOptions, StorageBackedSession, StorageBackedSessionOptions};
use maho_agent::types::{AgentMessage, CustomAgentMessage};
use maho_ai::types::{Api, AssistantMessage, Message, ProviderId, StopReason, Usage, UsageCost};

const NOW: i64 = 1_700_000_000_000;
const ENTRY_ID: &str = "00000000-0000-7000-8000-000000000001";

fn metadata() -> SessionMetadata {
    SessionMetadata {
        id: "session".to_owned(),
        created_at: NOW,
        storage_version: 1,
        cwd: Some("/workspace".to_owned()),
        parent_session_id: None,
        legacy_parent_session_path: None,
    }
}

fn zero_usage() -> Usage {
    Usage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 0,
        cost: UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
        },
    }
}

fn memory() -> Arc<dyn Storage> {
    Arc::new(MemoryStorage::new(MemoryStorageOptions {
        now: Some(Arc::new(|| NOW)),
    }))
}

fn session_with(storage: Arc<dyn Storage>) -> Arc<StorageBackedSession> {
    let session = Arc::new(StorageBackedSession::new(
        metadata(),
        storage,
        StorageBackedSessionOptions::default(),
    ));
    session.attach();
    session
}

async fn commit_session(session: &StorageBackedSession, transaction: Vec<Write>) -> Result<maho_agent::harness::session::types::CommitResult, maho_agent::harness::session::SessionError> {
    session
        .mutate(
            Arc::new(move |mutator, context| {
                let transaction = transaction.clone();
                Box::pin(async move {
                    mutator.commit(transaction, context).await?;
                    Ok(serde_json::Value::Null)
                })
            }),
            &BACKGROUND_CONTEXT,
        )
        .await?;
    Ok(maho_agent::harness::session::types::CommitResult {
        first_seq: 0,
        seqs: Vec::new(),
        timestamp: NOW,
        stats: Default::default(),
    })
}

#[tokio::test]
async fn delegates_typed_values_directly_without_validation_or_cloning() {
    let storage = Arc::new(InstrumentedStorage::new(memory()));
    let session = session_with(storage.clone());
    let data = serde_json::json!({ "nested": ["original"] });
    let scalar = value("test.value", "state").expect("address");

    session
        .mutate(
            Arc::new({
                let data = data.clone();
                let scalar = scalar.clone();
                move |mutator, context| {
                    let data = data.clone();
                    let scalar = scalar.clone();
                    Box::pin(async move {
                        let stored = data.clone();
                        mutator
                            .commit(
                                vec![
                                    insert_entry(NewEntry::custom_with_data(ENTRY_ID, None, "note", data)),
                                    Write::Value(set_value(&scalar, stored)),
                                ],
                                context,
                            )
                            .await?;
                        Ok(serde_json::Value::Null)
                    })
                }
            }),
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("commit");

    assert_eq!(storage.get_commit_attempts().len(), 1);
    let entries = session
        .get_entries(vec![ENTRY_ID.to_owned()], &BACKGROUND_CONTEXT)
        .await
        .expect("entries");
    let entry = entries.get(ENTRY_ID).expect("entry");
    assert_eq!(entry.timestamp, NOW);
    match &entry.kind {
        EntryKind::Custom { data, .. } => {
            assert_eq!(data.clone(), Some(serde_json::json!({ "nested": ["original"] })));
        }
        other => panic!("expected custom entry, got {other:?}"),
    }
    let stored = session
        .get_value(&scalar, &BACKGROUND_CONTEXT)
        .await
        .expect("value")
        .expect("stored");
    assert_eq!(stored.value, data);
    session.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn composes_bound_values_and_lists_atomically_with_entries_and_usage() {
    let storage = Arc::new(InstrumentedStorage::new(memory()));
    let session = session_with(storage.clone());
    let scalar = value("test.application.scalar", "").expect("address");
    let events = list("test.application.events", "").expect("address");

    let result = session
        .mutate(
            Arc::new({
                let scalar = scalar.clone();
                let events = events.clone();
                move |mutator, context| {
                    let scalar = scalar.clone();
                    let events = events.clone();
                    Box::pin(async move {
                        mutator
                            .commit(
                                vec![
                                    insert_entry(NewEntry::custom(ENTRY_ID, None, "note")),
                                    Write::Value(set_value(&scalar, serde_json::json!("state"))),
                                    Write::List(append_list(&events, serde_json::json!("event"))),
                                    insert_usage(NewUsageRow {
                                        id: "usage".to_owned(),
                                        usage: Usage {
                                            input: 1,
                                            output: 1,
                                            total_tokens: 2,
                                            ..zero_usage()
                                        },
                                        entry_id: None,
                                        adjustment: false,
                                        details: None,
                                    }),
                                ],
                                context,
                            )
                            .await?;
                        Ok(serde_json::Value::Null)
                    })
                }
            }),
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("commit");
    let _ = result;

    let stored = session
        .get_value(&scalar, &BACKGROUND_CONTEXT)
        .await
        .expect("value")
        .expect("stored");
    assert_eq!(stored.seq, 2);
    let elements = session.read_list(&events, None, &BACKGROUND_CONTEXT).await.expect("list");
    assert_eq!(elements.len(), 1);
    assert_eq!(elements[0].value, serde_json::json!("event"));
    assert_eq!(elements[0].seq, 3);
    assert_eq!(storage.get_commit_attempts().len(), 1);
    session.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn serializes_read_modify_write_callbacks_on_the_single_session_line() {
    let session = session_with(memory());
    let counter = value("test.counter", "").expect("address");

    let increment = |session: Arc<StorageBackedSession>, counter: maho_agent::harness::session::values::Value| async move {
        session
            .mutate(
                Arc::new(move |mutator, context| {
                    let counter = counter.clone();
                    Box::pin(async move {
                        let current = mutator
                            .get_value(&counter, context)
                            .await?
                            .and_then(|stored| stored.value.as_i64())
                            .unwrap_or(0);
                        mutator
                            .commit(vec![Write::Value(set_value(&counter, serde_json::json!(current + 1)))], context)
                            .await?;
                        Ok(serde_json::json!(current + 1))
                    })
                }),
                &BACKGROUND_CONTEXT,
            )
            .await
    };
    let (first, second) = tokio::join!(
        increment(session.clone(), counter.clone()),
        increment(session.clone(), counter.clone())
    );
    assert_eq!(first.expect("first"), serde_json::json!(1));
    assert_eq!(second.expect("second"), serde_json::json!(2));
    let stored = session
        .get_value(&counter, &BACKGROUND_CONTEXT)
        .await
        .expect("value")
        .expect("stored");
    assert_eq!(stored.value, serde_json::json!(2));
    session.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn keeps_separate_direct_reads_and_writes_deliberately_non_atomic() {
    let session = session_with(memory());
    let counter = value("test.counter", "").expect("address");
    let barrier = Arc::new(tokio::sync::Barrier::new(2));

    let increment = |session: Arc<StorageBackedSession>,
                     counter: maho_agent::harness::session::values::Value,
                     barrier: Arc<tokio::sync::Barrier>| async move {
        let current = session
            .get_value(&counter, &BACKGROUND_CONTEXT)
            .await
            .expect("read")
            .and_then(|stored| stored.value.as_i64())
            .unwrap_or(0);
        barrier.wait().await;
        session
            .set_value(&counter, serde_json::json!(current + 1), &BACKGROUND_CONTEXT)
            .await
            .expect("write");
    };
    tokio::join!(
        increment(session.clone(), counter.clone(), barrier.clone()),
        increment(session.clone(), counter.clone(), barrier.clone())
    );
    let stored = session
        .get_value(&counter, &BACKGROUND_CONTEXT)
        .await
        .expect("value")
        .expect("stored");
    assert_eq!(stored.value, serde_json::json!(1));
    session.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn queues_a_nested_public_writer_until_its_owning_callback_returns() {
    let session = session_with(memory());
    let nested: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>> = Arc::new(Mutex::new(None));
    let started = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));

    session
        .mutate(
            Arc::new({
                let session = session.clone();
                let nested = nested.clone();
                let started = started.clone();
                let finished = finished.clone();
                move |_mutator, _context| {
                    let session = session.clone();
                    let started = started.clone();
                    let finished = finished.clone();
                    let nested = nested.clone();
                    Box::pin(async move {
                        let finished_for_task = finished.clone();
                        let handle = tokio::spawn(async move {
                            started.store(true, Ordering::SeqCst);
                            session.set_name(Some("nested".to_owned()), &BACKGROUND_CONTEXT).await.expect("nested");
                            finished_for_task.store(true, Ordering::SeqCst);
                        });
                        *nested.lock().expect("nested") = Some(handle);
                        tokio::task::yield_now().await;
                        assert!(!finished.load(Ordering::SeqCst));
                        Ok(serde_json::Value::Null)
                    })
                }
            }),
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("mutate");

    let handle = nested.lock().expect("nested").take().expect("nested writer");
    handle.await.expect("nested task");
    assert!(started.load(Ordering::SeqCst));
    assert_eq!(session.get_name(&BACKGROUND_CONTEXT).await.expect("name"), Some("nested".to_owned()));
    session.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn exposes_either_side_of_an_atomic_multi_write_commit_to_direct_reads() {
    let first = value("test.atomic", "first").expect("address");
    let second = value("test.atomic", "second").expect("address");
    let storage = Arc::new(GatingStorage::new(memory()));
    storage
        .commit(
            vec![
                Write::Value(set_value(&first, serde_json::json!("old"))),
                Write::Value(set_value(&second, serde_json::json!("old"))),
            ],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("seed");
    storage.arm();
    let session = session_with(storage.clone());

    let committing = {
        let session = session.clone();
        let first = first.clone();
        let second = second.clone();
        tokio::spawn(async move {
            session
                .mutate(
                    Arc::new(move |mutator, context| {
                        let first = first.clone();
                        let second = second.clone();
                        Box::pin(async move {
                            mutator
                                .commit(
                                    vec![
                                        Write::Value(set_value(&first, serde_json::json!("new"))),
                                        Write::Value(set_value(&second, serde_json::json!("new"))),
                                    ],
                                    context,
                                )
                                .await?;
                            Ok(serde_json::Value::Null)
                        })
                    }),
                    &BACKGROUND_CONTEXT,
                )
                .await
                .expect("commit");
        })
    };
    storage.wait_pending(1).await.expect("pending");

    assert_eq!(
        session.get_value(&first, &BACKGROUND_CONTEXT).await.expect("read").expect("value").value,
        serde_json::json!("old")
    );
    assert_eq!(
        session.get_value(&second, &BACKGROUND_CONTEXT).await.expect("read").expect("value").value,
        serde_json::json!("old")
    );
    storage.next(1).await.expect("release");
    committing.await.expect("committing");
    assert_eq!(
        session.get_value(&first, &BACKGROUND_CONTEXT).await.expect("read").expect("value").value,
        serde_json::json!("new")
    );
    assert_eq!(
        session.get_value(&second, &BACKGROUND_CONTEXT).await.expect("read").expect("value").value,
        serde_json::json!("new")
    );
    session.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn holds_the_explicit_session_barrier_through_commit_until_end() {
    let storage = Arc::new(InstrumentedStorage::new(memory()));
    let session = session_with(storage.clone());
    let mutation = session.begin_mutation(&BACKGROUND_CONTEXT).await.expect("mutation");
    let queued_started = Arc::new(AtomicBool::new(false));
    let queued = session.mutate(
        Arc::new({
            let queued_started = queued_started.clone();
            move |_mutator, _context| {
                queued_started.store(true, Ordering::SeqCst);
                Box::pin(async move { Ok(serde_json::Value::Null) })
            }
        }),
        &BACKGROUND_CONTEXT,
    );

    tokio::task::yield_now().await;
    assert!(!queued_started.load(Ordering::SeqCst));
    assert!(mutation.get_value(&session_name(), &BACKGROUND_CONTEXT).await.expect("read").is_none());
    let result = mutation.commit(Vec::new(), &BACKGROUND_CONTEXT).await.expect("commit");
    assert!(result.seqs.is_empty());
    assert!(!queued_started.load(Ordering::SeqCst));
    assert!(mutation.get_value(&session_name(), &BACKGROUND_CONTEXT).await.expect("read").is_none());
    mutation.end(&BACKGROUND_CONTEXT).await;
    queued.await.expect("queued");
    assert!(queued_started.load(Ordering::SeqCst));
    assert_eq!(storage.get_commit_attempts(), vec![Vec::<Write>::new()]);
    assert_eq!(
        mutation.get_entries(Vec::new(), &BACKGROUND_CONTEXT).await.expect_err("invalidated").kind,
        SessionErrorKind::MutatorInactive
    );
    assert_eq!(
        mutation.commit(Vec::new(), &BACKGROUND_CONTEXT).await.expect_err("invalidated").kind,
        SessionErrorKind::MutatorInactive
    );
    mutation.end(&BACKGROUND_CONTEXT).await;
    session.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn allows_direct_reads_to_observe_a_committed_value_before_the_mutation_scope_ends() {
    let session = session_with(memory());
    let mutation = session.begin_mutation(&BACKGROUND_CONTEXT).await.expect("mutation");
    assert!(session.get_value(&session_name(), &BACKGROUND_CONTEXT).await.expect("read").is_none());
    mutation
        .commit(
            vec![Write::Value(set_value(&session_name(), serde_json::json!("visible")))],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("commit");
    let queued_started = Arc::new(AtomicBool::new(false));
    let queued = session.mutate(
        Arc::new({
            let queued_started = queued_started.clone();
            move |_mutator, _context| {
                queued_started.store(true, Ordering::SeqCst);
                Box::pin(async move { Ok(serde_json::Value::Null) })
            }
        }),
        &BACKGROUND_CONTEXT,
    );

    assert_eq!(
        session.get_value(&session_name(), &BACKGROUND_CONTEXT).await.expect("read").expect("value").value,
        serde_json::json!("visible")
    );
    assert!(!queued_started.load(Ordering::SeqCst));
    mutation.end(&BACKGROUND_CONTEXT).await;
    queued.await.expect("queued");
    assert!(queued_started.load(Ordering::SeqCst));
    session.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn ends_an_explicit_mutation_without_committing_and_lets_close_finish() {
    let storage = Arc::new(InstrumentedStorage::new(memory()));
    let session = session_with(storage.clone());
    let mutation = session.begin_mutation(&BACKGROUND_CONTEXT).await.expect("mutation");
    let closed = Arc::new(AtomicBool::new(false));

    let mut closing = Box::pin(session.close(&BACKGROUND_CONTEXT));
    assert!(futures::poll!(closing.as_mut()).is_pending(), "close must wait for the explicit scope");
    assert!(!closed.load(Ordering::SeqCst));
    mutation.end(&BACKGROUND_CONTEXT).await;
    closing.await;
    closed.store(true, Ordering::SeqCst);
    assert!(closed.load(Ordering::SeqCst));
    assert!(storage.get_commit_attempts().is_empty());
}

#[tokio::test]
async fn exposes_explicit_branch_scans_through_the_session_and_callback_scoped_mutator() {
    let session = session_with(memory());
    let child_id = "00000000-0000-7000-8000-000000000002";
    commit_session(
        &session,
        vec![
            insert_entry(NewEntry::custom(ENTRY_ID, None, "root")),
            insert_entry(NewEntry::custom(child_id, Some(ENTRY_ID.to_owned()), "child")),
        ],
    )
    .await
    .expect("seed");

    let entries = session
        .scan_branch(
            maho_agent::harness::session::types::StorageBranchScan {
                order: Some(maho_agent::harness::session::types::BranchOrder::OldestFirst),
                ..maho_agent::harness::session::types::StorageBranchScan::new(child_id)
            },
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("branch");
    assert_eq!(
        entries.iter().map(|entry| entry.id.as_str()).collect::<Vec<&str>>(),
        vec![ENTRY_ID, child_id]
    );

    let captured: Arc<Mutex<Option<Arc<dyn maho_agent::harness::session::types::SessionMutation>>>> =
        Arc::new(Mutex::new(None));
    session
        .mutate(
            Arc::new({
                let _captured = captured.clone();
                move |mutator, _context| {
                    Box::pin(async move {
                        let limited = mutator
                            .scan_branch(
                                maho_agent::harness::session::types::StorageBranchScan {
                                    limit: Some(1),
                                    ..maho_agent::harness::session::types::StorageBranchScan::new(child_id)
                                },
                                &BACKGROUND_CONTEXT,
                            )
                            .await
                            .expect("mutator scan");
                        assert_eq!(limited.len(), 1);
                        assert_eq!(limited[0].id, child_id);
                        Ok(serde_json::Value::Null)
                    })
                }
            }),
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("mutate");
    assert!(captured.lock().expect("captured").is_none());
    session.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn rejects_pending_assistant_entries_at_the_durable_session_write_boundary() {
    let storage = Arc::new(InstrumentedStorage::new(memory()));
    let session = session_with(storage.clone());
    let pending = AgentMessage::Llm(Message::Assistant(Box::new(AssistantMessage {
        content: Vec::new(),
        api: Api::from("anthropic-messages"),
        provider: ProviderId::from("anthropic"),
        model: String::new(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        diagnostics: None,
        usage: zero_usage(),
        stop_reason: StopReason::Pending,
        stop_details: None,
        deferred: None,
        error_message: None,
        abort_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: NOW,
    })));

    let error = session
        .mutate(
            Arc::new(move |mutator, context| {
                let pending = pending.clone();
                Box::pin(async move {
                    mutator
                        .commit(vec![insert_entry(NewEntry::message(ENTRY_ID, None, pending))], context)
                        .await?;
                    Ok(serde_json::Value::Null)
                })
            }),
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect_err("pending assistant message");
    assert_eq!(error.kind, SessionErrorKind::PendingAssistantMessage);
    assert!(storage.get_commit_attempts().is_empty());
    assert!(session
        .get_entries(vec![ENTRY_ID.to_owned()], &BACKGROUND_CONTEXT)
        .await
        .expect("entries")
        .is_empty());
    session.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn trusts_typed_custom_messages_without_repository_schema_registration() {
    let session = session_with(memory());
    let message = AgentMessage::Custom(CustomAgentMessage::Custom(
        maho_agent::harness::messages::create_custom_message(
            "notice",
            maho_agent::harness::messages::CustomMessageContent::Text("maintenance".to_owned()),
            true,
            None,
            NOW,
        ),
    ));
    let entry = NewEntry::message(ENTRY_ID, None, message.clone());

    commit_session(&session, vec![insert_entry(entry.clone())]).await.expect("commit");

    let entries = session
        .get_entries(vec![ENTRY_ID.to_owned()], &BACKGROUND_CONTEXT)
        .await
        .expect("entries");
    let stored = entries.get(ENTRY_ID).expect("entry");
    assert_eq!(stored.id, entry.id);
    assert_eq!(stored.parent_id, entry.parent_id);
    assert_eq!(stored.kind, entry.kind);
    assert_eq!(stored.timestamp, NOW);
    session.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn serializes_mutations_permits_one_commit_attempt_and_invalidates_the_mutator() {
    let storage = Arc::new(InstrumentedStorage::new(memory()));
    let session = session_with(storage.clone());

    session
        .mutate(
            Arc::new(move |mutator, context| {
                Box::pin(async move {
                    assert!(mutator.get_value(&session_name(), context).await?.is_none());
                    mutator
                        .commit(
                            vec![Write::Value(set_value(&session_name(), serde_json::json!("committed")))],
                            context,
                        )
                        .await?;
                    let second = mutator.commit(Vec::new(), context).await.expect_err("second commit");
                    assert_eq!(second.kind, SessionErrorKind::MutatorCommitAttempted);
                    Ok(serde_json::Value::Null)
                })
            }),
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("mutate");

    assert_eq!(storage.get_commit_attempts().len(), 1);
    assert_eq!(session.get_name(&BACKGROUND_CONTEXT).await.expect("name"), Some("committed".to_owned()));
    session.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn consumes_the_commit_guard_when_the_first_commit_fails() {
    let storage = Arc::new(InstrumentedStorage::new(memory()));
    let session = session_with(storage.clone());

    session
        .mutate(
            Arc::new(move |mutator, context| {
                Box::pin(async move {
                    let failed = mutator
                        .commit(
                            vec![insert_entry(NewEntry::custom(ENTRY_ID, Some("missing".to_owned()), "note"))],
                            context,
                        )
                        .await
                        .expect_err("missing parent");
                    assert_eq!(failed.kind, SessionErrorKind::MissingParent);
                    let second = mutator.commit(Vec::new(), context).await.expect_err("second commit");
                    assert_eq!(second.kind, SessionErrorKind::MutatorCommitAttempted);
                    Ok(serde_json::Value::Null)
                })
            }),
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("mutate");
    assert_eq!(storage.get_commit_attempts().len(), 1);
    session.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn mints_distinct_follower_ids_with_the_leader_timestamp() {
    let session = session_with(memory());
    let leader_timestamp = 0x0123_4567_89ab;
    let generator = session.id_generator().clone();
    let leader = generator(Some(leader_timestamp));
    let followers = [generator(Some(leader_timestamp)), generator(Some(leader_timestamp))];
    let decode_timestamp = |id: &str| {
        let compact: String = id.chars().filter(|character| *character != '-').collect();
        i64::from_str_radix(&compact[..12], 16).expect("timestamp")
    };
    assert_eq!(
        [leader.clone(), followers[0].clone(), followers[1].clone()]
            .iter()
            .map(|id| decode_timestamp(id))
            .collect::<Vec<i64>>(),
        vec![leader_timestamp, leader_timestamp, leader_timestamp]
    );
    let unique: std::collections::HashSet<String> = [leader, followers[0].clone(), followers[1].clone()].into_iter().collect();
    assert_eq!(unique.len(), 3);
    session.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn accepts_an_injected_id_generator_for_deterministic_execution_tests() {
    let counter = Arc::new(AtomicUsize::new(0));
    let generator: maho_agent::harness::session::types::IdGenerator = Arc::new({
        let counter = counter.clone();
        move |timestamp_ms: Option<i64>| {
            let next = counter.fetch_add(1, Ordering::SeqCst) + 1;
            match timestamp_ms {
                Some(timestamp) => format!("{timestamp}:{next}"),
                None => format!("now:{next}"),
            }
        }
    });
    let session = session_with_with_generator(memory(), generator.clone());

    assert_eq!(generator(Some(7)), "7:1");
    assert_eq!(session.id_generator()(None), "now:2");
    session.close(&BACKGROUND_CONTEXT).await;
}

fn session_with_with_generator(storage: Arc<dyn Storage>, generator: maho_agent::harness::session::types::IdGenerator) -> Arc<StorageBackedSession> {
    let session = Arc::new(StorageBackedSession::new(
        metadata(),
        storage,
        StorageBackedSessionOptions {
            id_generator: Some(generator),
            ..StorageBackedSessionOptions::default()
        },
    ));
    session.attach();
    session
}

#[tokio::test]
async fn exposes_metadata_directly_and_the_shared_uuidv7_id_generator() {
    let session = session_with(memory());
    assert_eq!(session.metadata().id, "session");
    assert_eq!(session.metadata().created_at, NOW);
    let id = session.id_generator()(None);
    assert_eq!(id.len(), 36);
    assert_eq!(id.chars().nth(14), Some('7'));
    session.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn closes_idempotently_and_rejects_operations_not_admitted_before_close() {
    let session = session_with(memory());

    tokio::join!(session.close(&BACKGROUND_CONTEXT), session.close(&BACKGROUND_CONTEXT));
    assert_eq!(
        session
            .mutate(
                Arc::new(|_mutator, _context| Box::pin(async move { Ok(serde_json::Value::Null) })),
                &BACKGROUND_CONTEXT
            )
            .await
            .expect_err("closed")
            .kind,
        SessionErrorKind::Closed
    );
    assert!(session.get_entries(Vec::new(), &BACKGROUND_CONTEXT).await.is_err());
    assert!(session.get_value(&session_name(), &BACKGROUND_CONTEXT).await.is_err());
    assert!(session.scan_values(&session_name(), &BACKGROUND_CONTEXT).await.is_err());
    assert!(session
        .scan_branch(
            maho_agent::harness::session::types::StorageBranchScan::new(ENTRY_ID),
            &BACKGROUND_CONTEXT
        )
        .await
        .is_err());
    let _ = session_invariant_error("unused");
    let _ = EntryScan::default();
}
