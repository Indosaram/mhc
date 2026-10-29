use super::*;
use serde_json::json;

fn document(path: &str) -> MentionDocument {
    MentionDocument { path: path.to_string() }
}

fn entry(id: &str, text: &str) -> Value {
    json!({ "id": id, "type": "message", "message": { "role": "user", "content": text } })
}

#[test]
fn given_a_mention_when_the_window_is_scanned_then_only_the_mentioned_path_is_excluded() {
    let documents = [document("notes/a.md"), document("notes/b.md")];
    let window = [entry("e1", "see notes/a.md for the rollback order")];
    let excluded = excluded_paths_by_window_scan(&window, &documents);
    assert_eq!(excluded, BTreeSet::from(["notes/a.md".to_string()]));
}

#[test]
fn given_a_path_followed_by_a_word_character_when_scanned_then_it_is_not_a_mention() {
    let documents = [document("notes/a.md")];
    let window = [entry("e1", "notes/a.mdX is not the file")];
    assert!(excluded_paths_by_window_scan(&window, &documents).is_empty());
}

#[test]
fn given_a_path_at_the_end_of_an_entry_when_scanned_then_it_is_a_mention() {
    let documents = [document("notes/a.md")];
    let window = [entry("e1", "notes/a.md")];
    assert_eq!(
        excluded_paths_by_window_scan(&window, &documents),
        BTreeSet::from(["notes/a.md".to_string()])
    );
}

#[test]
fn given_an_incremental_index_when_compared_then_it_matches_the_window_scan() {
    let documents = [document("notes/a.md"), document("notes/b.md"), document("notes/c.md")];
    let window = [
        entry("e1", "unrelated chatter"),
        entry("e2", "the deploy gate is in notes/b.md"),
        entry("e3", "notes/c.md was renamed"),
    ];
    let mut index = create_transcript_mention_index();
    let indexed = index.excluded_paths(TranscriptMentionInput {
        session_id: "s1",
        entries: &window,
        documents: &documents,
    });
    assert_eq!(indexed, excluded_paths_by_window_scan(&window, &documents));
    assert_eq!(
        indexed,
        BTreeSet::from(["notes/b.md".to_string(), "notes/c.md".to_string()])
    );
}

#[test]
fn given_a_streaming_newest_entry_when_it_grows_then_the_next_scan_sees_the_new_text() {
    let documents = [document("notes/a.md")];
    let mut index = create_transcript_mention_index();
    let first = [entry("e1", "nothing here"), entry("e2", "still nothing")];
    assert!(index
        .excluded_paths(TranscriptMentionInput { session_id: "s1", entries: &first, documents: &documents })
        .is_empty());
    // The newest entry is never cached, so the appended mention is observed on the next scan.
    let grown = [entry("e1", "nothing here"), entry("e2", "now see notes/a.md")];
    assert_eq!(
        index.excluded_paths(TranscriptMentionInput { session_id: "s1", entries: &grown, documents: &documents }),
        BTreeSet::from(["notes/a.md".to_string()])
    );
}

#[test]
fn given_a_seam_unsafe_path_when_scanned_then_the_whole_window_fallback_is_used() {
    let documents = [document("weird}]path.md")];
    let window = [entry("e1", "weird}]path.md is mentioned")];
    let mut index = create_transcript_mention_index();
    let indexed = index.excluded_paths(TranscriptMentionInput {
        session_id: "s1",
        entries: &window,
        documents: &documents,
    });
    assert_eq!(indexed, excluded_paths_by_window_scan(&window, &documents));
    assert_eq!(indexed, BTreeSet::from(["weird}]path.md".to_string()]));
}

#[test]
fn given_more_than_the_session_cap_when_indexing_then_old_sessions_are_evicted() {
    let documents = [document("notes/a.md")];
    let mut index = create_transcript_mention_index();
    for number in 0..(MAX_TRACKED_SESSIONS + 3) {
        let session = format!("s{number}");
        let window = [entry(&format!("e{number}"), "nothing")];
        index.excluded_paths(TranscriptMentionInput { session_id: &session, entries: &window, documents: &documents });
    }
    assert_eq!(index.sessions.len(), MAX_TRACKED_SESSIONS);
    assert_eq!(index.sessions.first().map(|(id, _)| id.as_str()), Some("s3"));
}

#[test]
fn given_a_moved_corpus_when_indexing_then_the_session_cache_is_dropped_and_rebuilt() {
    let documents = [document("notes/a.md")];
    let window = [entry("e1", "nothing"), entry("e2", "nothing")];
    let mut index = create_transcript_mention_index();
    index.excluded_paths(TranscriptMentionInput { session_id: "s1", entries: &window, documents: &documents });
    let moved = [document("notes/a.md"), document("notes/b.md")];
    let mentioned = [entry("e1", "nothing"), entry("e2", "see notes/b.md")];
    assert_eq!(
        index.excluded_paths(TranscriptMentionInput { session_id: "s1", entries: &mentioned, documents: &moved }),
        BTreeSet::from(["notes/b.md".to_string()])
    );
}
