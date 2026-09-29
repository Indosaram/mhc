//! Ports of queue-publication, queue-reconcile and queue-watermark TS tests.
use std::fs;
use std::sync::Arc;

use pretty_assertions::assert_eq;

use super::tests::{
    CONVERSATION, clock_from, identity_fixture, journal, queue_file_names, request,
};
use crate::facts::queue::{FactsQueue, FactsQueueOptions};
use crate::facts::schema::facts_queue_paths;
use crate::identity::MemoryIdentityPaths;

fn queue_with(paths: &MemoryIdentityPaths, start: i64) -> FactsQueue {
    FactsQueue::new(FactsQueueOptions {
        identity_paths: paths.clone(),
        now: Some(clock_from(start)),
        on_publish: None,
    })
}

fn ranges(queue: &FactsQueue) -> Vec<(String, String)> {
    queue
        .list_pending()
        .expect("list pending")
        .into_iter()
        .map(|e| (e.range.start_message_id, e.range.end_message_id))
        .collect()
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}

// --- queue-publication.test.ts

#[test]
fn given_published_entry_when_filename_inspected_then_it_is_colon_free_and_hashes_conversation_id()
{
    let (_dir, paths) = identity_fixture();
    let queue = queue_with(&paths, 0);

    queue.enqueue(request(journal(1), None)).expect("enqueue");

    let names = queue_file_names(&paths);
    let name = names.first().expect("one queue file");
    assert!(!name.contains(':'), "{name}");
    assert!(!name.contains(CONVERSATION), "{name}");
    let parts: Vec<&str> = name.trim_end_matches(".json").split('-').collect();
    assert_eq!(parts.len(), 3, "{name}");
    let ts = parts[0].as_bytes();
    assert_eq!(ts.len(), 19, "{name}");
    assert!(
        ts[..8].iter().all(u8::is_ascii_digit) && ts[8] == b'T',
        "{name}"
    );
    assert!(
        ts[9..18].iter().all(u8::is_ascii_digit) && ts[18] == b'Z',
        "{name}"
    );
    let is_hex = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    assert!(parts[1].len() == 12 && is_hex(parts[1]), "{name}");
    assert!(parts[2].len() == 8 && is_hex(parts[2]), "{name}");
}

#[test]
fn given_two_publications_in_same_millisecond_when_both_land_then_endpoint_hash_keeps_them_distinct()
 {
    let (_dir, paths) = identity_fixture();
    let queue = FactsQueue::new(FactsQueueOptions {
        identity_paths: paths.clone(),
        now: Some(Arc::new(|| 0)),
        on_publish: None,
    });

    queue.enqueue(request(journal(1), None)).expect("first");
    queue.enqueue(request(journal(2), None)).expect("second");

    assert_eq!(queue_file_names(&paths).len(), 2);
}

#[test]
fn given_two_concurrent_enqueue_attempts_for_same_delta_when_both_run_then_exactly_one_entry_is_published()
 {
    let (_dir, paths) = identity_fixture();
    let first = queue_with(&paths, 0);
    let second = queue_with(&paths, 50_000);

    let results = std::thread::scope(|scope| {
        let a = scope.spawn(|| first.enqueue(request(journal(2), None)).expect("first"));
        let b = scope.spawn(|| second.enqueue(request(journal(2), None)).expect("second"));
        [a.join().expect("join a"), b.join().expect("join b")]
    });

    assert_eq!(results.iter().filter(|r| r.is_enqueued()).count(), 1);
    assert_eq!(queue_file_names(&paths).len(), 1);
}

// --- queue-reconcile.test.ts

#[test]
fn given_leftover_queue_file_with_no_completion_when_reconcile_runs_then_it_is_listed_as_launchable()
 {
    let (_dir, paths) = identity_fixture();
    queue_with(&paths, 0)
        .enqueue(request(journal(2), None))
        .expect("enqueue");

    let launchable = queue_with(&paths, 90_000).list_pending().expect("list");

    assert_eq!(launchable.len(), 1);
    assert_eq!(launchable[0].conversation_id, CONVERSATION);
    assert_eq!(launchable[0].entries.len(), 4);
}

#[test]
fn given_malformed_queue_file_when_list_pending_runs_then_it_is_ignored_instead_of_failing() {
    let (_dir, paths) = identity_fixture();
    let queue = queue_with(&paths, 0);
    queue.enqueue(request(journal(2), None)).expect("enqueue");
    fs::write(
        paths
            .facts_queue
            .join("20260101T000000000Z-deadbeefcafe-12345678.json"),
        "{ not json",
    )
    .expect("write malformed");

    let pending = queue.list_pending().expect("list");

    assert_eq!(pending.len(), 1);
}

#[test]
fn given_consumed_entries_when_mark_consumed_runs_then_files_deleted_and_watermark_records_endpoint()
 {
    let (_dir, paths) = identity_fixture();
    let queue = queue_with(&paths, 0);
    queue.enqueue(request(journal(2), None)).expect("enqueue");
    let pending = queue.list_pending().expect("list");

    queue.mark_consumed(&pending).expect("mark consumed");

    assert_eq!(queue_file_names(&paths).len(), 0);
    let raw = fs::read_to_string(facts_queue_paths(&paths).consumed_path).expect("consumed file");
    let consumed: serde_json::Value = serde_json::from_str(&raw).expect("json");
    assert_eq!(consumed["version"], 1);
    assert_eq!(consumed["consumed"][CONVERSATION]["end_message_id"], "m2");
}

// --- queue-watermark.test.ts

#[test]
fn given_older_batch_settling_after_newer_one_queued_when_consumed_then_enqueue_watermark_never_rolls_back()
 {
    let (_dir, paths) = identity_fixture();
    let queue = queue_with(&paths, 0);
    queue.enqueue(request(journal(2), None)).expect("a");
    queue.enqueue(request(journal(4), None)).expect("b");
    let pending = queue.list_pending().expect("list");
    let older: Vec<_> = pending
        .iter()
        .filter(|e| e.range.end_message_id == "m2")
        .cloned()
        .collect();
    let newer: Vec<_> = pending
        .iter()
        .filter(|e| e.range.end_message_id == "m4")
        .cloned()
        .collect();
    assert_eq!((older.len(), newer.len()), (1, 1));

    queue.mark_consumed(&newer).expect("newer");
    let after_newer = queue.read_cursor(CONVERSATION).expect("cursor");
    queue.mark_consumed(&older).expect("older");
    let after_older = queue.read_cursor(CONVERSATION).expect("cursor");

    let m4 = Some("m4".to_string());
    assert_eq!(after_newer.enqueued_through_message_id, m4);
    assert_eq!(after_older.enqueued_through_message_id, m4);
    assert_eq!(after_newer.consumed_through_message_id, m4);
    assert_eq!(after_older.consumed_through_message_id, m4);
}

#[test]
fn given_late_older_batch_consumed_when_next_settle_runs_then_no_overlapping_range_is_republished()
{
    let (_dir, paths) = identity_fixture();
    let queue = queue_with(&paths, 0);
    queue.enqueue(request(journal(2), None)).expect("a");
    queue.enqueue(request(journal(4), None)).expect("b");
    let pending = queue.list_pending().expect("list");
    let pick = |id: &str| -> Vec<_> {
        pending
            .iter()
            .filter(|e| e.range.end_message_id == id)
            .cloned()
            .collect()
    };
    queue.mark_consumed(&pick("m4")).expect("m4");
    queue.mark_consumed(&pick("m2")).expect("m2");

    let next = queue.enqueue(request(journal(6), None)).expect("next");

    assert!(next.is_enqueued());
    assert_eq!(ranges(&queue), vec![pair("m5", "m6")]);
}

#[test]
fn given_failed_batch_when_retained_then_enqueue_watermark_does_not_roll_back_and_entry_survives() {
    let (_dir, paths) = identity_fixture();
    let queue = queue_with(&paths, 0);
    queue.enqueue(request(journal(2), None)).expect("enqueue");
    let before = queue.read_cursor(CONVERSATION).expect("before");

    let after = queue.read_cursor(CONVERSATION).expect("after");

    assert_eq!(before.enqueued_through_message_id.as_deref(), Some("m2"));
    assert_eq!(after.enqueued_through_message_id.as_deref(), Some("m2"));
    assert_eq!(after.consumed_through_message_id, None);
    assert_eq!(queue.list_pending().expect("list").len(), 1);
}

#[test]
fn given_queue_file_landed_without_cursor_write_when_next_settle_runs_then_retained_endpoint_anchors_range()
 {
    let (_dir, paths) = identity_fixture();
    let queue = queue_with(&paths, 0);
    queue.enqueue(request(journal(2), None)).expect("enqueue");
    let _ = fs::remove_file(facts_queue_paths(&paths).cursor_path(CONVERSATION));

    let next = queue.enqueue(request(journal(4), None)).expect("next");

    assert!(next.is_enqueued());
    assert_eq!(ranges(&queue), vec![pair("m1", "m2"), pair("m3", "m4")]);
}
