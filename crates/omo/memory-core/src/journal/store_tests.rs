use pretty_assertions::assert_eq;
use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

use super::*;
use crate::journal::entries::{ProjectedToolCall, TranscriptProjection};
use crate::journal::lock::{
    JournalLockTimeoutError, try_reclaim_stale_lock, with_local_journal_lock,
};

fn create_journal(dir: &Path) -> TranscriptJournal {
    let mut options = TranscriptJournalOptions::new(dir);
    options.now = Some(Box::new(|| "2026-08-09T12:00:00.000Z".to_string()));
    TranscriptJournal::new(options)
}

#[test]
fn test_rows_are_idempotent_and_steps_stay_derived_when_three_completed_assistant_messages_reconciled_twice()
 {
    // given
    let temp_dir = tempfile::tempdir().unwrap();
    let journal = create_journal(temp_dir.path());
    let messages = vec![
        TranscriptProjection::User {
            message_id: "user-1".to_string(),
            text: "help".to_string(),
        },
        TranscriptProjection::Assistant {
            message_id: "assistant-1".to_string(),
            text_blocks: vec!["one".to_string()],
            reasoning_blocks: Vec::new(),
            tool_calls: Vec::new(),
        },
        TranscriptProjection::Assistant {
            message_id: "assistant-tool".to_string(),
            text_blocks: Vec::new(),
            reasoning_blocks: Vec::new(),
            tool_calls: vec![ProjectedToolCall {
                call_id: "tool-1".to_string(),
                name: Some("read".to_string()),
                args_text: None,
                result_text: None,
                result_ok: None,
            }],
        },
        TranscriptProjection::Assistant {
            message_id: "assistant-2".to_string(),
            text_blocks: vec!["two".to_string()],
            reasoning_blocks: Vec::new(),
            tool_calls: Vec::new(),
        },
        TranscriptProjection::Assistant {
            message_id: "assistant-3".to_string(),
            text_blocks: vec!["three".to_string()],
            reasoning_blocks: Vec::new(),
            tool_calls: Vec::new(),
        },
    ];

    // when
    let first = journal.reconcile(&messages).unwrap();
    let second = journal.reconcile(&messages).unwrap();

    // then
    assert_eq!(
        first,
        AppendResult {
            appended: 5,
            skipped: 0
        }
    );
    assert_eq!(
        second,
        AppendResult {
            appended: 0,
            skipped: 5
        }
    );

    let content = std::fs::read_to_string(&journal.transcript_path).unwrap();
    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 5);

    let state = journal.get_state().unwrap();
    assert_eq!(state.total_completed_steps, 3);
    assert_eq!(state.reflected_completed_steps, 0);
    assert_eq!(state.steps_since_last_successful_reflection, 3);
}

#[test]
fn test_counter_is_recomputed_when_stale_derived_counter_on_disk_written() {
    // given
    let temp_dir = tempfile::tempdir().unwrap();
    let journal = create_journal(temp_dir.path());
    journal
        .reconcile(&[TranscriptProjection::Assistant {
            message_id: "assistant-1".to_string(),
            text_blocks: vec!["one".to_string()],
            reasoning_blocks: Vec::new(),
            tool_calls: Vec::new(),
        }])
        .unwrap();

    let stale_state = serde_json::json!({
        "schema_version": "v3_assistant_steps",
        "total_completed_steps": 1,
        "reflected_completed_steps": 0,
        "steps_since_last_successful_reflection": 99,
    });
    std::fs::write(
        &journal.state_path,
        format!("{}\n", serde_json::to_string(&stale_state).unwrap()),
    )
    .unwrap();

    // when
    journal.set_pending_compaction(true).unwrap();

    // then
    let raw = std::fs::read_to_string(&journal.state_path).unwrap();
    let on_disk: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        on_disk.get("steps_since_last_successful_reflection"),
        Some(&serde_json::json!(1))
    );
    assert_eq!(
        on_disk.get("pending_compaction"),
        Some(&serde_json::json!(true))
    );
}

#[test]
fn test_settles_and_leaves_no_lock_behind_when_flush_runs_on_journal_with_rows() {
    // given
    let temp_dir = tempfile::tempdir().unwrap();
    let journal = create_journal(temp_dir.path());
    journal
        .reconcile(&[TranscriptProjection::Assistant {
            message_id: "assistant-1".to_string(),
            text_blocks: vec!["one".to_string()],
            reasoning_blocks: Vec::new(),
            tool_calls: Vec::new(),
        }])
        .unwrap();

    // when
    journal.flush(None).unwrap();

    // then
    assert_eq!(journal.lock_path.exists(), false);
    let content = std::fs::read_to_string(&journal.transcript_path).unwrap();
    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1);
}

#[test]
fn test_throws_abort_error_and_creates_no_lock_file_when_signal_aborted_before_acquisition() {
    // given
    let temp_dir = tempfile::tempdir().unwrap();
    let journal = create_journal(temp_dir.path());
    journal
        .reconcile(&[TranscriptProjection::Assistant {
            message_id: "assistant-1".to_string(),
            text_blocks: vec!["one".to_string()],
            reasoning_blocks: Vec::new(),
            tool_calls: Vec::new(),
        }])
        .unwrap();
    let is_aborted = || true;

    // when
    let failure = journal.flush(Some(&is_aborted));

    // then
    assert!(matches!(failure, Err(JournalError::Aborted)));
    assert_eq!(journal.lock_path.exists(), false);
}

#[test]
fn test_throws_without_acquiring_when_abort_during_acquisition_retry_loop_on_contended_lock() {
    // given
    let temp_dir = tempfile::tempdir().unwrap();
    let lock_path = temp_dir.path().join("state.lock");
    std::fs::write(&lock_path, "held-by-another-holder\n").unwrap();
    let aborted = Arc::new(AtomicBool::new(false));
    let aborted_clone = Arc::clone(&aborted);
    let mut task_runs = 0;

    let is_aborted = move || {
        aborted_clone.store(true, Ordering::Relaxed);
        true
    };

    // when
    let failure = with_local_journal_lock(
        &lock_path,
        || {
            task_runs += 1;
            Ok(())
        },
        Some(&is_aborted),
    );

    // then
    assert!(matches!(failure, Err(JournalError::Aborted)));
    assert_eq!(task_runs, 0);
    assert_eq!(
        std::fs::read_to_string(&lock_path).unwrap(),
        "held-by-another-holder\n"
    );
}

#[test]
fn test_lock_file_is_released_anyway_when_abort_raised_while_task_holds_lock() {
    // given
    let temp_dir = tempfile::tempdir().unwrap();
    let lock_path = temp_dir.path().join("state.lock");
    let aborted = Arc::new(AtomicBool::new(false));
    let aborted_clone = Arc::clone(&aborted);

    // when
    let result = with_local_journal_lock(
        &lock_path,
        || {
            aborted_clone.store(true, Ordering::Relaxed);
            assert_eq!(lock_path.exists(), true);
            Ok(())
        },
        Some(&move || aborted.load(Ordering::Relaxed)),
    );

    // then
    assert!(result.is_ok());
    assert_eq!(lock_path.exists(), false);
}

#[test]
fn test_stale_lock_is_reclaimed_and_task_runs_when_lock_file_left_by_dead_process() {
    // given
    let temp_dir = tempfile::tempdir().unwrap();
    let lock_path = temp_dir.path().join("state.lock");

    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .spawn()
        .unwrap();
    let dead_pid = child.id();
    let _ = child.wait();

    std::fs::write(&lock_path, format!("{dead_pid}\n")).unwrap();

    // when
    let mut task_runs = 0;
    with_local_journal_lock(
        &lock_path,
        || {
            task_runs += 1;
            let payload = std::fs::read_to_string(&lock_path).unwrap();
            let current_pid = std::process::id();
            assert!(payload.starts_with(&format!("{current_pid}\n")));
            Ok(())
        },
        None,
    )
    .unwrap();

    // then
    assert_eq!(task_runs, 1);
    assert_eq!(lock_path.exists(), false);
}

#[test]
fn test_artifact_is_reclaimed_by_age_when_payload_free_lock_file_abandoned_mid_acquisition() {
    // given
    let temp_dir = tempfile::tempdir().unwrap();
    let lock_path = temp_dir.path().join("state.lock");
    let file = File::create(&lock_path).unwrap();
    drop(file);

    let _ = std::process::Command::new("touch")
        .args(["-t", "202001010000", lock_path.to_str().unwrap()])
        .status();

    // when
    let mut task_runs = 0;
    with_local_journal_lock(
        &lock_path,
        || {
            task_runs += 1;
            Ok(())
        },
        None,
    )
    .unwrap();

    // then
    assert_eq!(task_runs, 1);
    assert_eq!(lock_path.exists(), false);
}

#[test]
fn test_throws_and_live_lock_never_stolen_when_lock_file_owned_by_live_process_and_deadline_passes()
{
    // given
    let temp_dir = tempfile::tempdir().unwrap();
    let lock_path = temp_dir.path().join("state.lock");
    let current_pid = std::process::id();
    std::fs::write(&lock_path, format!("{current_pid}\n")).unwrap();

    // when
    try_reclaim_stale_lock(&lock_path);

    // then
    assert_eq!(lock_path.exists(), true);
    assert_eq!(
        std::fs::read_to_string(&lock_path).unwrap(),
        format!("{current_pid}\n")
    );
}

#[test]
fn test_flush_returns_early_without_throwing_when_signal_aborted_mid_flush() {
    // given
    let temp_dir = tempfile::tempdir().unwrap();
    let dir = temp_dir.path().to_path_buf();
    let journal = create_journal(&dir);
    journal
        .reconcile(&[TranscriptProjection::Assistant {
            message_id: "assistant-1".to_string(),
            text_blocks: vec!["one".to_string()],
            reasoning_blocks: Vec::new(),
            tool_calls: Vec::new(),
        }])
        .unwrap();

    let aborted = Arc::new(AtomicBool::new(false));
    let aborted_clone = Arc::clone(&aborted);

    struct CustomLock {
        aborted: Arc<AtomicBool>,
    }
    impl JournalLock for CustomLock {
        fn with_lock(
            &self,
            lock_path: &Path,
            task: &mut dyn FnMut() -> Result<(), JournalError>,
            cancel: Option<&dyn Fn() -> bool>,
        ) -> Result<(), JournalError> {
            let aborted = Arc::clone(&self.aborted);
            with_local_journal_lock(
                lock_path,
                || {
                    aborted.store(true, Ordering::Relaxed);
                    task()
                },
                cancel,
            )
        }
    }

    let mut options = TranscriptJournalOptions::new(&dir);
    options.lock = Some(Arc::new(CustomLock {
        aborted: aborted_clone,
    }));
    let aborting = TranscriptJournal::new(options);

    // when
    let result = aborting.flush(Some(&move || aborted.load(Ordering::Relaxed)));

    // then
    assert!(result.is_ok());
    assert_eq!(dir.join("state.lock").exists(), false);
}

#[test]
fn test_every_contender_still_runs_serialized_when_holder_outlasts_acquisition_wait_on_overlapping_same_process_acquisitions()
 {
    // given
    let temp_dir = tempfile::tempdir().unwrap();
    let lock_path = temp_dir.path().join("state.lock");
    let order = Arc::new(Mutex::new(Vec::new()));
    let barrier = Arc::new(Barrier::new(2));

    let order_a = Arc::clone(&order);
    let lock_a = lock_path.clone();
    let barrier_a = Arc::clone(&barrier);
    let handle_a = thread::spawn(move || {
        with_local_journal_lock(
            &lock_a,
            || {
                order_a.lock().unwrap().push("a-start".to_string());
                barrier_a.wait();
                order_a.lock().unwrap().push("a-end".to_string());
                Ok(())
            },
            None,
        )
        .unwrap();
    });

    barrier.wait();

    let order_b = Arc::clone(&order);
    let lock_b = lock_path.clone();
    let handle_b = thread::spawn(move || {
        with_local_journal_lock(
            &lock_b,
            || {
                order_b.lock().unwrap().push("b".to_string());
                Ok(())
            },
            None,
        )
        .unwrap();
    });

    let order_c = Arc::clone(&order);
    let lock_c = lock_path.clone();
    let handle_c = thread::spawn(move || {
        with_local_journal_lock(
            &lock_c,
            || {
                order_c.lock().unwrap().push("c".to_string());
                Ok(())
            },
            None,
        )
        .unwrap();
    });

    // when
    handle_a.join().unwrap();
    handle_b.join().unwrap();
    handle_c.join().unwrap();

    // then
    let recorded = order.lock().unwrap().clone();
    assert_eq!(recorded[0], "a-start");
    assert_eq!(recorded[1], "a-end");
    assert!(recorded.contains(&"b".to_string()));
    assert!(recorded.contains(&"c".to_string()));
    assert_eq!(recorded.len(), 4);
    assert_eq!(lock_path.exists(), false);
}

#[test]
fn test_reused_pid_lock_is_reclaimed_when_stale_lock_whose_live_pid_belongs_to_different_process() {
    // given
    let temp_dir = tempfile::tempdir().unwrap();
    let lock_path = temp_dir.path().join("state.lock");
    let current_pid = std::process::id();
    std::fs::write(
        &lock_path,
        format!("{current_pid}\nps-lstart: Jan  1 00:00:00 2000\n"),
    )
    .unwrap();

    // when
    let mut task_runs = 0;
    with_local_journal_lock(
        &lock_path,
        || {
            task_runs += 1;
            Ok(())
        },
        None,
    )
    .unwrap();

    // then
    assert_eq!(task_runs, 1);
    assert_eq!(lock_path.exists(), false);
}

#[test]
fn test_typed_timeout_error_is_thrown_when_live_foreign_owner_keeps_lock_and_deadline_expires() {
    // given
    let temp_dir = tempfile::tempdir().unwrap();
    let lock_path = temp_dir.path().join("state.lock");

    // when
    let err = JournalLockTimeoutError::new(&lock_path);

    // then
    assert_eq!(err.retriable, true);
    assert_eq!(
        format!("{err}"),
        format!(
            "Timed out acquiring transcript journal lock: {}",
            lock_path.display()
        )
    );
}
