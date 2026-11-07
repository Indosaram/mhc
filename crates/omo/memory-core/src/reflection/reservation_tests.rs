use pretty_assertions::assert_eq;

use std::sync::Arc;
use tempfile::TempDir;

use crate::identity::layout::build_identity_paths;
use crate::identity::resolve::MemoryIdentity;
use crate::journal::store::TranscriptJournal;

use crate::reflection::machine::{
    CapturedConversation, ReflectionOutcome, ReflectionRequest, ReflectionTrigger, TriggerConfig,
};
use crate::reflection::reservation::{
    ReflectionLauncherIdentity, ReflectionReservationStore, ReflectionReservationStoreOptions,
    ReservationError,
};

fn setup_store(temp: &TempDir) -> ReflectionReservationStore {
    let root = temp.path().join("mem-root");
    let paths = build_identity_paths(&root, "test-identity");
    std::fs::create_dir_all(&paths.reflection).expect("create reflection dir");
    std::fs::create_dir_all(&paths.locks).expect("create locks dir");

    let identity = MemoryIdentity {
        id: "test-identity".to_string(),
        safe_slug: "test-identity".to_string(),
        paths,
    };

    let journals_root = temp.path().join("journals");
    let get_journal =
        Arc::new(
            move |conversation_id: &str| -> Result<
                TranscriptJournal,
                crate::reflection::reservation::ReservationError,
            > {
                let store_dir = journals_root.join(conversation_id);
                std::fs::create_dir_all(&store_dir).expect("create journal dir");
                Ok(TranscriptJournal::new(
                    crate::journal::TranscriptJournalOptions::new(store_dir),
                ))
            },
        );

    let launcher_identity = Arc::new(|| ReflectionLauncherIdentity {
        pid: 1234,
        hostname: "test-host".to_string(),
        process_start: Some("2026-03-30T00:00:00Z".to_string()),
    });

    ReflectionReservationStore::new(ReflectionReservationStoreOptions {
        identity,
        config: TriggerConfig {
            step_count: Some(5),
            on_compaction: Some(true),
            snapshot_max_bytes: None,
        },
        get_journal,
        create_run_id: None,
        now_iso: Some(Arc::new(|| "2026-03-30T12:00:00Z".to_string())),
        launcher_identity: Some(launcher_identity),
    })
}

fn make_request(trigger: ReflectionTrigger, convo: &str) -> ReflectionRequest {
    ReflectionRequest {
        trigger,
        origin: None,
        conversation_ids: vec![convo.to_string()],
        snapshots: vec![CapturedConversation {
            conversation_id: convo.to_string(),
            snapshot: crate::journal::cursor::ReflectionSnapshot {
                start_message_id: "m0".to_string(),
                end_message_id: "m1".to_string(),
                start_line: 0,
                end_snapshot_line: 1,
                backlog_remaining: None,
                entries: Vec::new(),
            },
        }],
        focus: None,
        recent_n: None,
        target_doc: None,
    }
}

#[test]
fn given_an_active_reflection_run_when_a_manual_run_arrives_then_manual_work_is_queued_into_pending()
 {
    let temp = TempDir::new().expect("temp dir");
    let store = setup_store(&temp);

    let res1 = store
        .try_reserve(make_request(ReflectionTrigger::StepCount, "convo-1"), None)
        .expect("reserve 1");
    assert_eq!(res1.status, "active");
    assert_eq!(res1.run.launcher_pid, Some(1234));

    let res2 = store
        .try_reserve(make_request(ReflectionTrigger::Manual, "convo-2"), None)
        .expect("reserve 2");
    assert_eq!(res2.status, "pending");

    let state = store.read_state().expect("read state");
    assert!(state.active.is_some());
    assert_eq!(state.active.as_ref().unwrap().run_id, res1.run.run_id);
    assert!(state.pending.is_some());
    assert_eq!(
        state.pending.as_ref().unwrap().request.trigger,
        ReflectionTrigger::Manual
    );
}

#[test]
fn given_active_reflection_and_queued_compaction_when_manual_reflection_arrives_then_manual_overrides_compaction_in_pending()
 {
    let temp = TempDir::new().expect("temp dir");
    let store = setup_store(&temp);

    store
        .try_reserve(make_request(ReflectionTrigger::StepCount, "convo-1"), None)
        .expect("active");
    store
        .try_reserve(make_request(ReflectionTrigger::Compaction, "convo-2"), None)
        .expect("pending compaction");

    let state1 = store.read_state().expect("read state 1");
    assert_eq!(
        state1.pending.as_ref().unwrap().request.trigger,
        ReflectionTrigger::Compaction
    );

    store
        .try_reserve(make_request(ReflectionTrigger::Manual, "convo-3"), None)
        .expect("pending manual");

    let state2 = store.read_state().expect("read state 2");
    assert_eq!(
        state2.pending.as_ref().unwrap().request.trigger,
        ReflectionTrigger::Manual
    );
}

#[test]
fn given_completed_active_run_when_pending_exists_then_pending_is_promoted_to_active() {
    let temp = TempDir::new().expect("temp dir");
    let store = setup_store(&temp);

    let active_res = store
        .try_reserve(make_request(ReflectionTrigger::Manual, "convo-1"), None)
        .expect("active");
    let pending_res = store
        .try_reserve(make_request(ReflectionTrigger::Manual, "convo-2"), None)
        .expect("pending");

    let completion = store
        .complete(&active_res.run.run_id, ReflectionOutcome::Merged)
        .expect("complete");

    assert_eq!(completion.outcome, ReflectionOutcome::Merged);
    assert!(completion.launch.is_some());
    assert_eq!(
        completion.launch.as_ref().unwrap().run_id,
        pending_res.run.run_id
    );

    let state = store.read_state().expect("read state");
    assert_eq!(
        state.active.as_ref().unwrap().run_id,
        pending_res.run.run_id
    );
    assert!(state.pending.is_none());
}

#[test]
fn given_dream_outcomes_when_runs_complete_then_dream_state_advances_only_for_merged_and_no_changes()
 {
    // Port of reservation.test.ts "#given dream outcomes #when runs complete #then dream state
    // advances only for merged and no_changes". A fresh store per outcome keeps the assertion
    // independent of iteration order (the pinned test relies on failed/timed_out running first).
    let outcomes = [
        ReflectionOutcome::Failed,
        ReflectionOutcome::TimedOut,
        ReflectionOutcome::Merged,
        ReflectionOutcome::NoChanges,
    ];
    for outcome in outcomes {
        let temp = TempDir::new().expect("temp dir");
        let store = setup_store(&temp);
        let active = store
            .try_reserve(make_request(ReflectionTrigger::Dream, "convo-1"), None)
            .expect("reserve dream");

        store
            .complete(&active.run.run_id, outcome)
            .expect("complete");

        let state_path = build_identity_paths(&temp.path().join("mem-root"), "test-identity")
            .runtime
            .join("dream")
            .join("state.json");
        if outcome == ReflectionOutcome::Failed || outcome == ReflectionOutcome::TimedOut {
            assert!(
                !state_path.exists(),
                "dream state must not be written for {outcome:?}"
            );
        } else {
            let raw = std::fs::read_to_string(&state_path).expect("read dream state");
            let parsed: serde_json::Value = serde_json::from_str(&raw).expect("parse dream state");
            assert_eq!(
                parsed,
                serde_json::json!({
                    "last_dream_at": "2026-03-30T12:00:00Z",
                    "lastRunId": active.run.run_id,
                })
            );
        }
    }
}

#[test]
fn given_a_non_dream_active_run_when_it_completes_then_no_dream_state_is_written() {
    let temp = TempDir::new().expect("temp dir");
    let store = setup_store(&temp);
    let active = store
        .try_reserve(make_request(ReflectionTrigger::Manual, "convo-1"), None)
        .expect("reserve manual");

    store
        .complete(&active.run.run_id, ReflectionOutcome::Merged)
        .expect("complete");

    let state_path = build_identity_paths(&temp.path().join("mem-root"), "test-identity")
        .runtime
        .join("dream")
        .join("state.json");
    assert!(!state_path.exists());
}

#[test]
fn given_an_aborted_signal_when_reservation_starts_then_active_and_pending_state_remain_empty() {
    // Port of reservation.test.ts "#given an aborted signal #when reservation starts #then active
    // and pending state remain empty".
    let temp = TempDir::new().expect("temp dir");
    let store = setup_store(&temp);
    let aborted = || true;
    let aborted: &dyn Fn() -> bool = &aborted;

    let error = store
        .try_reserve(
            make_request(ReflectionTrigger::StepCount, "convo-1"),
            Some(aborted),
        )
        .expect_err("aborted reservation must fail");

    assert!(matches!(error, ReservationError::Aborted));
    let state = store.read_state().expect("read state");
    assert!(state.active.is_none());
    assert!(state.pending.is_none());
}

#[test]
fn given_a_live_signal_when_reservation_runs_then_it_still_reserves() {
    let temp = TempDir::new().expect("temp dir");
    let store = setup_store(&temp);
    let live = || false;
    let live: &dyn Fn() -> bool = &live;

    let result = store
        .try_reserve(
            make_request(ReflectionTrigger::StepCount, "convo-1"),
            Some(live),
        )
        .expect("live reservation");

    assert_eq!(result.status, "active");
    assert!(store.read_state().expect("read state").active.is_some());
}

/// reservation.test.ts `fixture()`: one journal (`conversation-a`) holding two completed
/// assistant steps, a fixed clock, a fixed launcher identity and sequential run ids.
fn journal_fixture(
    temp: &TempDir,
    step_count: usize,
) -> (
    ReflectionReservationStore,
    TranscriptJournal,
    std::path::PathBuf,
) {
    use crate::journal::{TranscriptJournalOptions, TranscriptProjection};
    let root = temp.path().join("mem-root");
    let paths = build_identity_paths(&root, "agent-test");
    std::fs::create_dir_all(&paths.reflection).expect("create reflection dir");
    std::fs::create_dir_all(&paths.locks).expect("create locks dir");
    let reflection_dir = paths.reflection.clone();
    let journal_dir = paths.transcripts.join("conversation-a");
    std::fs::create_dir_all(&journal_dir).expect("journal dir");
    let journal = TranscriptJournal::new(TranscriptJournalOptions::new(&journal_dir));
    journal
        .reconcile(&[
            TranscriptProjection::Assistant {
                message_id: "assistant-1".into(),
                text_blocks: vec!["one".into()],
                reasoning_blocks: Vec::new(),
                tool_calls: Vec::new(),
            },
            TranscriptProjection::Assistant {
                message_id: "assistant-2".into(),
                text_blocks: vec!["two".into()],
                reasoning_blocks: Vec::new(),
                tool_calls: Vec::new(),
            },
        ])
        .expect("reconcile");
    let identity = MemoryIdentity {
        id: "agent-test".into(),
        safe_slug: "agent-test".into(),
        paths,
    };
    let next_id = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let get_journal_dir = journal_dir.clone();
    let store = ReflectionReservationStore::new(ReflectionReservationStoreOptions {
        identity,
        config: TriggerConfig {
            step_count: Some(step_count),
            on_compaction: Some(true),
            snapshot_max_bytes: None,
        },
        get_journal: Arc::new(move |id: &str| {
            if id != "conversation-a" {
                return Err(ReservationError::Journal(format!("missing journal: {id}")));
            }
            Ok(TranscriptJournal::new(TranscriptJournalOptions::new(
                &get_journal_dir,
            )))
        }),
        create_run_id: Some(Arc::new(move || {
            format!(
                "run-{}",
                next_id.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
            )
        })),
        now_iso: Some(Arc::new(|| "2026-08-10T00:00:00.000Z".to_string())),
        launcher_identity: Some(Arc::new(|| ReflectionLauncherIdentity {
            pid: 4242,
            hostname: "fixture-host".into(),
            process_start: Some("fixture-start".into()),
        })),
    });
    (store, journal, reflection_dir)
}

fn captured(journal: &TranscriptJournal, trigger: ReflectionTrigger) -> ReflectionRequest {
    let snapshot = journal
        .capture_reflection_snapshot(None)
        .expect("capture")
        .expect("expected snapshot");
    ReflectionRequest {
        trigger,
        origin: None,
        conversation_ids: vec!["conversation-a".into()],
        snapshots: vec![CapturedConversation {
            conversation_id: "conversation-a".into(),
            snapshot,
        }],
        focus: None,
        recent_n: None,
        target_doc: None,
    }
}

#[test]
fn given_journal_state_at_threshold_when_the_persisted_machine_evaluates_a_successful_settle_then_it_captures_and_reserves_the_cursor_snapshot()
 {
    let temp = TempDir::new().expect("temp dir");
    let (store, _, _) = journal_fixture(&temp, 2);
    let result = store
        .evaluate(
            "conversation-a",
            crate::reflection::machine::ReflectionEvent::Settled { success: true },
        )
        .expect("evaluate")
        .expect("reservation");
    assert_eq!(result.status, "active");
    assert_eq!(result.run.request.trigger, ReflectionTrigger::StepCount);
    assert_eq!(
        result.run.request.snapshots[0].snapshot.end_message_id,
        "assistant-2"
    );
}

#[test]
fn given_a_new_active_reservation_when_it_is_published_then_launch_owner_identity_and_reservation_time_are_durable_in_the_same_record()
 {
    let temp = TempDir::new().expect("temp dir");
    let (store, journal, reflection_dir) = journal_fixture(&temp, 2);
    let result = store
        .try_reserve(captured(&journal, ReflectionTrigger::StepCount), None)
        .expect("reserve");
    assert_eq!(
        result.run.reserved_at.as_deref(),
        Some("2026-08-10T00:00:00.000Z")
    );
    assert_eq!(result.run.launcher_pid, Some(4242));
    assert_eq!(
        result.run.launcher_hostname.as_deref(),
        Some("fixture-host")
    );
    assert_eq!(
        result.run.launcher_process_start.as_deref(),
        Some("fixture-start")
    );
    let persisted: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(reflection_dir.join("active.lock")).expect("active.lock"),
    )
    .expect("json");
    assert_eq!(
        persisted,
        serde_json::to_value(&result.run).expect("run json")
    );
}

#[test]
fn given_simultaneous_trigger_requests_when_reserved_then_one_becomes_active_and_all_others_collapse_into_one_atomic_pending_record()
 {
    let temp = TempDir::new().expect("temp dir");
    let (store, journal, reflection_dir) = journal_fixture(&temp, 2);
    let base = captured(&journal, ReflectionTrigger::StepCount);
    let first = store.try_reserve(base.clone(), None).expect("first");
    let (second, third) = std::thread::scope(|scope| {
        let compaction = ReflectionRequest {
            trigger: ReflectionTrigger::Compaction,
            conversation_ids: vec!["conversation-c".into()],
            ..base.clone()
        };
        let manual = ReflectionRequest {
            trigger: ReflectionTrigger::Manual,
            conversation_ids: vec!["conversation-b".into()],
            focus: Some("remember names".into()),
            recent_n: Some(5),
            ..base.clone()
        };
        let a = scope.spawn(|| store.try_reserve(compaction, None).expect("second"));
        let b = scope.spawn(|| store.try_reserve(manual, None).expect("third"));
        (
            a.join().expect("second thread"),
            b.join().expect("third thread"),
        )
    });
    let state = store.read_state().expect("state");
    assert_eq!(first.status, "active");
    assert_eq!(second.status, "pending");
    assert_eq!(third.status, "pending");
    assert!(state.active.is_some());
    let pending = state.pending.expect("pending");
    assert_eq!(pending.request.trigger, ReflectionTrigger::Manual);
    assert_eq!(pending.request.focus.as_deref(), Some("remember names"));
    assert_eq!(pending.request.recent_n, Some(5));
    assert!(
        std::fs::read_to_string(reflection_dir.join("pending.json"))
            .expect("pending.json")
            .contains("\"trigger\": \"manual\"")
    );
}

#[test]
fn given_a_failed_active_run_when_completed_then_its_cursor_is_unchanged_and_valid_pending_work_is_promoted()
 {
    let temp = TempDir::new().expect("temp dir");
    let (store, journal, _) = journal_fixture(&temp, 2);
    let request = captured(&journal, ReflectionTrigger::StepCount);
    let active = store.try_reserve(request.clone(), None).expect("active");
    let pending = store
        .try_reserve(
            ReflectionRequest {
                trigger: ReflectionTrigger::Manual,
                focus: Some("retry manually".into()),
                ..request
            },
            None,
        )
        .expect("pending");
    let completion = store
        .complete(&active.run.run_id, ReflectionOutcome::Failed)
        .expect("complete");
    assert_eq!(pending.status, "pending");
    assert_eq!(
        journal
            .get_state()
            .expect("state")
            .reflected_completed_steps,
        0
    );
    assert_eq!(
        completion.launch.as_ref().map(|run| run.run_id.as_str()),
        Some(pending.run.run_id.as_str())
    );
    assert_eq!(
        store
            .read_state()
            .expect("state")
            .active
            .map(|run| run.run_id),
        Some(pending.run.run_id)
    );
}

#[test]
fn given_successful_compaction_triggered_work_when_completed_then_its_snapshot_advances_and_the_compaction_flag_clears_exactly_once()
 {
    let temp = TempDir::new().expect("temp dir");
    let (store, journal, _) = journal_fixture(&temp, 2);
    journal.set_pending_compaction(true).expect("flag");
    let active = store
        .try_reserve(captured(&journal, ReflectionTrigger::Compaction), None)
        .expect("active");
    let completion = store
        .complete(&active.run.run_id, ReflectionOutcome::Merged)
        .expect("complete");
    let state = journal.get_state().expect("state");
    assert!(completion.launch.is_none());
    assert_eq!(state.reflected_completed_steps, 2);
    assert_eq!(state.pending_compaction, Some(false));
    assert!(store.read_state().expect("state").active.is_none());
}

#[test]
fn given_a_pending_threshold_request_made_stale_by_successful_cursor_advancement_when_dequeued_then_it_is_dropped()
 {
    let temp = TempDir::new().expect("temp dir");
    let (store, journal, _) = journal_fixture(&temp, 2);
    let request = captured(&journal, ReflectionTrigger::StepCount);
    let active = store.try_reserve(request.clone(), None).expect("active");
    store.try_reserve(request, None).expect("pending");
    let completion = store
        .complete(&active.run.run_id, ReflectionOutcome::Merged)
        .expect("complete");
    assert!(completion.launch.is_none());
    assert_eq!(
        journal
            .get_state()
            .expect("state")
            .steps_since_last_successful_reflection,
        0
    );
    assert!(store.read_state().expect("state").active.is_none());
}

#[test]
fn given_automatic_pending_work_below_its_threshold_when_dequeued_then_it_is_dropped_but_a_manual_pending_request_remains_launchable()
 {
    let temp = TempDir::new().expect("temp dir");
    let (store, journal, _) = journal_fixture(&temp, 3);
    let bare = |trigger| ReflectionRequest {
        trigger,
        origin: None,
        conversation_ids: vec!["conversation-a".into()],
        snapshots: Vec::new(),
        focus: None,
        recent_n: None,
        target_doc: None,
    };
    let stale = CapturedConversation {
        conversation_id: "conversation-a".into(),
        snapshot: crate::journal::cursor::ReflectionSnapshot {
            start_message_id: "a".into(),
            end_message_id: "b".into(),
            start_line: 0,
            end_snapshot_line: 0,
            backlog_remaining: None,
            entries: Vec::new(),
        },
    };
    let active = store
        .try_reserve(bare(ReflectionTrigger::Manual), None)
        .expect("active");
    store
        .try_reserve(
            ReflectionRequest {
                snapshots: vec![stale],
                ..bare(ReflectionTrigger::StepCount)
            },
            None,
        )
        .expect("pending");
    assert!(
        store
            .complete(&active.run.run_id, ReflectionOutcome::Failed)
            .expect("complete")
            .launch
            .is_none()
    );
    let next = store
        .try_reserve(bare(ReflectionTrigger::StepCount), None)
        .expect("next");
    let manual = store
        .try_reserve(
            ReflectionRequest {
                focus: Some("always".into()),
                ..bare(ReflectionTrigger::Manual)
            },
            None,
        )
        .expect("manual");
    let completion = store
        .complete(&next.run.run_id, ReflectionOutcome::Failed)
        .expect("complete next");
    assert_eq!(
        completion.launch.map(|run| run.run_id),
        Some(manual.run.run_id)
    );
    assert_eq!(
        journal
            .get_state()
            .expect("state")
            .reflected_completed_steps,
        0
    );
}
