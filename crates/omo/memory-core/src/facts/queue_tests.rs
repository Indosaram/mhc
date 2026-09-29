use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use pretty_assertions::assert_eq;
use tempfile::tempdir;

use crate::identity::MemoryIdentityPaths;
use crate::identity::layout::build_identity_paths;
use crate::journal::entries::{TextTranscriptEntry, ToolCallTranscriptEntry, TranscriptEntry};

use crate::facts::queue::{
    FactsAbortSignal, FactsEnqueueRequest, FactsEnqueueResult, FactsQueue, FactsQueueOptions,
};

const IDENTITY: &str = "facts-queue-agent";
pub(super) const CONVERSATION: &str = "conversation-alpha";

pub(super) fn identity_fixture() -> (tempfile::TempDir, MemoryIdentityPaths) {
    let dir = tempdir().expect("tempdir");
    let memory_root = dir.path().join("memory");
    let paths = build_identity_paths(&memory_root, IDENTITY);
    (dir, paths)
}

fn entry_user(message_id: &str, text: &str) -> TranscriptEntry {
    TranscriptEntry::Text(TextTranscriptEntry::new(
        "user",
        text,
        "2026-01-01T00:00:00.000Z",
        format!("{message_id}:user"),
        message_id,
    ))
}

fn entry_assistant(message_id: &str, text: &str) -> TranscriptEntry {
    TranscriptEntry::Text(TextTranscriptEntry::new(
        "assistant",
        text,
        "2026-01-01T00:00:00.000Z",
        format!("{message_id}:assistant"),
        message_id,
    ))
}

pub(super) fn journal(count: usize) -> Vec<TranscriptEntry> {
    let mut entries = Vec::with_capacity(count * 2);
    for index in 1..=count {
        entries.push(entry_user(&format!("m{index}"), &format!("ask {index}")));
        entries.push(entry_assistant(
            &format!("m{index}"),
            &format!("answer {index}"),
        ));
    }
    entries
}

pub(super) fn request(
    entries: Vec<TranscriptEntry>,
    signal: Option<FactsAbortSignal>,
) -> FactsEnqueueRequest {
    FactsEnqueueRequest {
        identity: IDENTITY.to_string(),
        session_id: "session-1".to_string(),
        conversation_id: CONVERSATION.to_string(),
        entries,
        signal,
    }
}

pub(super) fn clock_from(start: i64) -> Arc<dyn Fn() -> i64 + Send + Sync> {
    let tick = AtomicI64::new(start);
    Arc::new(move || tick.fetch_add(1000, Ordering::SeqCst) + 1000)
}

pub(super) fn queue_file_names(paths: &MemoryIdentityPaths) -> Vec<String> {
    let read_dir = match fs::read_dir(&paths.facts_queue) {
        Ok(dir) => dir,
        Err(_) => return Vec::new(),
    };
    let mut names = Vec::new();
    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".json") && name != "consumed.json" {
            names.push(name);
        }
    }
    names.sort();
    names
}

#[test]
fn test_given_a_fresh_journal_delta_when_enqueue_runs_then_one_queue_file_holds_the_full_canonical_range()
 {
    // given
    let (_dir, paths) = identity_fixture();
    let queue = FactsQueue::new(FactsQueueOptions {
        identity_paths: paths.clone(),
        now: Some(clock_from(0)),
        on_publish: None,
    });
    let entries = journal(2);
    let total_entries = entries.len();

    // when
    let result = queue
        .enqueue(request(entries, None))
        .expect("enqueue success");

    // then
    assert!(result.is_enqueued());
    let names = queue_file_names(&paths);
    assert_eq!(names.len(), 1);
    let pending = queue.list_pending().expect("list pending");
    assert_eq!(pending.len(), 1);
    let first = &pending[0];
    assert_eq!(first.version, 1);
    assert_eq!(first.conversation_id, CONVERSATION);
    assert_eq!(first.range.start_message_id, "m1");
    assert_eq!(first.range.end_message_id, "m2");
    assert_eq!(first.range.end_snapshot_line, total_entries as u64);
    let msg_ids: Vec<&str> = first
        .entries
        .iter()
        .map(|row| row.source_message_id())
        .collect();
    assert_eq!(msg_ids, vec!["m1", "m1", "m2", "m2"]);
}

#[test]
fn test_given_an_identical_settle_when_enqueue_runs_again_then_the_duplicate_endpoint_is_skipped() {
    // given
    let (_dir, paths) = identity_fixture();
    let queue = FactsQueue::new(FactsQueueOptions {
        identity_paths: paths.clone(),
        now: Some(clock_from(0)),
        on_publish: None,
    });
    let entries = journal(2);
    queue
        .enqueue(request(entries.clone(), None))
        .expect("first enqueue");

    // when
    let duplicate = queue
        .enqueue(request(entries, None))
        .expect("duplicate enqueue");

    // then
    assert_eq!(
        duplicate,
        FactsEnqueueResult::NotEnqueued {
            reason: "no-new-entries".to_string()
        }
    );
    assert_eq!(queue_file_names(&paths).len(), 1);
}

#[test]
fn test_given_a_retained_entry_ending_at_m2_when_a_later_settle_arrives_then_the_new_range_starts_strictly_after_it()
 {
    // given
    let (_dir, _paths) = identity_fixture();
    let queue = FactsQueue::new(FactsQueueOptions {
        identity_paths: _paths,
        now: Some(clock_from(0)),
        on_publish: None,
    });
    queue
        .enqueue(request(journal(2), None))
        .expect("first enqueue");

    // when
    let second = queue
        .enqueue(request(journal(4), None))
        .expect("second enqueue");

    // then
    assert!(second.is_enqueued());
    let pending = queue.list_pending().expect("list pending");
    assert_eq!(pending.len(), 2);
    let ranges: Vec<(&str, &str)> = pending
        .iter()
        .map(|item| {
            (
                item.range.start_message_id.as_str(),
                item.range.end_message_id.as_str(),
            )
        })
        .collect();
    assert_eq!(ranges, vec![("m1", "m2"), ("m3", "m4")]);
}

#[test]
fn test_given_the_consumed_watermark_already_covers_the_endpoint_when_enqueue_runs_then_nothing_is_published()
 {
    // given
    let (_dir, paths) = identity_fixture();
    let queue = FactsQueue::new(FactsQueueOptions {
        identity_paths: paths.clone(),
        now: Some(clock_from(0)),
        on_publish: None,
    });
    let entries = journal(2);
    queue
        .enqueue(request(entries.clone(), None))
        .expect("first enqueue");
    let pending = queue.list_pending().expect("list pending");
    queue.mark_consumed(&pending).expect("mark consumed");

    // when
    let again = queue
        .enqueue(request(entries, None))
        .expect("again enqueue");

    // then
    assert_eq!(
        again,
        FactsEnqueueResult::NotEnqueued {
            reason: "no-new-entries".to_string()
        }
    );
    assert_eq!(queue_file_names(&paths).len(), 0);
    assert_eq!(queue.list_pending().expect("list pending").len(), 0);
}

#[test]
fn test_given_only_non_canonical_trailing_entries_when_enqueue_runs_then_no_queue_file_is_published()
 {
    // given
    let (_dir, paths) = identity_fixture();
    let queue = FactsQueue::new(FactsQueueOptions {
        identity_paths: paths.clone(),
        now: Some(clock_from(0)),
        on_publish: None,
    });
    let entries = vec![TranscriptEntry::ToolCall(ToolCallTranscriptEntry::new(
        Some("read".to_string()),
        None,
        None,
        None,
        "2026-01-01T00:00:00.000Z",
        "m1:tool:call-1",
        "m1",
    ))];

    // when
    let result = queue.enqueue(request(entries, None)).expect("tool enqueue");

    // then
    assert_eq!(
        result,
        FactsEnqueueResult::NotEnqueued {
            reason: "no-new-entries".to_string()
        }
    );
    assert_eq!(queue_file_names(&paths).len(), 0);
}

#[test]
fn test_given_an_aborted_drain_signal_when_enqueue_is_attempted_then_nothing_publishes_and_the_cursor_stays_untouched()
 {
    // given
    let (_dir, _paths) = identity_fixture();
    let queue = FactsQueue::new(FactsQueueOptions {
        identity_paths: _paths,
        now: Some(clock_from(0)),
        on_publish: None,
    });
    let signal = FactsAbortSignal::new();
    signal.abort();

    // when
    let result = queue
        .enqueue(request(journal(2), Some(signal)))
        .expect("aborted enqueue");

    // then
    assert_eq!(
        result,
        FactsEnqueueResult::NotEnqueued {
            reason: "no-new-entries".to_string()
        }
    );
    assert_eq!(queue.list_pending().expect("list pending").len(), 0);
    let cursor = queue.read_cursor(CONVERSATION).expect("read cursor");
    assert_eq!(cursor.enqueued_through_message_id, None);
    assert_eq!(cursor.consumed_through_message_id, None);
}

#[test]
fn test_given_the_abort_fires_at_the_interior_clock_hook_when_enqueue_runs_then_nothing_publishes_and_the_cursor_stays_untouched()
 {
    // given
    let (_dir, _paths) = identity_fixture();
    let signal = FactsAbortSignal::new();
    let signal_clone = signal.clone();
    let queue = FactsQueue::new(FactsQueueOptions {
        identity_paths: _paths,
        now: Some(Arc::new(move || {
            signal_clone.abort();
            1_768_478_400_000
        })),
        on_publish: None,
    });

    // when
    let result = queue
        .enqueue(request(journal(2), Some(signal)))
        .expect("enqueue clock abort");

    // then
    assert_eq!(
        result,
        FactsEnqueueResult::NotEnqueued {
            reason: "no-new-entries".to_string()
        }
    );
    assert_eq!(queue.list_pending().expect("list pending").len(), 0);
    let cursor = queue.read_cursor(CONVERSATION).expect("read cursor");
    assert_eq!(cursor.enqueued_through_message_id, None);
}

#[test]
fn test_given_the_abort_fires_after_the_publish_lands_when_the_cursor_advance_is_attempted_then_the_batch_stays_retryable_with_the_watermark_unmoved()
 {
    // given
    let (_dir, _paths) = identity_fixture();
    let signal = FactsAbortSignal::new();
    let signal_clone = signal.clone();
    let queue = FactsQueue::new(FactsQueueOptions {
        identity_paths: _paths,
        now: Some(Arc::new(|| 1_768_478_400_000)),
        on_publish: Some(Arc::new(move || {
            signal_clone.abort();
        })),
    });

    // when
    let result = queue
        .enqueue(request(journal(2), Some(signal)))
        .expect("enqueue with publish hook");

    // then
    assert!(result.is_enqueued());
    assert_eq!(queue.list_pending().expect("list pending").len(), 1);
    let cursor = queue.read_cursor(CONVERSATION).expect("read cursor");
    assert_eq!(cursor.enqueued_through_message_id, None);
}
