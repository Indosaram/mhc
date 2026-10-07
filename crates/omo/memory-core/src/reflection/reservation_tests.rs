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
fn given_dream_outcomes_when_runs_complete_then_dream_state_advances_only_for_merged_and_no_changes() {
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
