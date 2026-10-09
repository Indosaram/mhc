use maho_omo_post_mutation::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn make_event(input: serde_json::Value, tool_name: &str) -> PostMutationEvent {
    PostMutationEvent::new(tool_name, input, None)
}

#[test]
fn test_mutation_tool_names_set() {
    assert_eq!(MUTATION_TOOL_NAMES, &["write", "edit", "apply_patch"]);
    assert!(is_mutation_tool_name("write"));
    assert!(is_mutation_tool_name("edit"));
    assert!(is_mutation_tool_name("apply_patch"));
    assert!(!is_mutation_tool_name("read"));
    assert!(!is_mutation_tool_name("bash"));
}

#[test]
fn test_extracts_all_files_from_apply_patch_result() {
    let input = json!({
        "input": "*** Begin Patch\n*** Update File: a.ts\n*** Add File: b.ts\n*** End Patch",
        "files": [
            { "filePath": "c.ts", "movePath": "d.ts" }
        ]
    });
    let event = make_event(input, "apply_patch");
    let extracted = extract_mutated_file_paths(&event);
    assert_eq!(extracted, vec!["a.ts", "b.ts", "c.ts", "d.ts"]);
}

#[test]
fn test_extracts_paths_and_preserves_whitespace_filenames() {
    let input = json!({
        "path": " file with spaces.rs ",
        "filePath": "second.rs",
        "paths": ["arr1.rs", "", "  tab\t.rs  "],
        "filePaths": ["arr3.rs", null, "arr1.rs"],
        "input": "*** Update File:   trimmed_patch.ts  \n*** Add File:   another.ts  "
    });
    let event = make_event(input, "write");
    let extracted = extract_mutated_file_paths(&event);
    // Direct/array paths preserve whitespace; empty string is excluded; raw patch prefix scans are trimmed
    assert_eq!(
        extracted,
        vec![
            " file with spaces.rs ",
            "second.rs",
            "arr1.rs",
            "  tab\t.rs  ",
            "arr3.rs",
            "trimmed_patch.ts",
            "another.ts"
        ]
    );
}

#[tokio::test]
async fn test_coalesces_duplicate_in_flight_paths() {
    let runs = Arc::new(AtomicUsize::new(0));
    let sf = SingleFlight::<i32>::new();

    let runs1 = Arc::clone(&runs);
    let sf1 = sf.clone();
    let first = tokio::spawn(async move {
        sf1.run("a.ts", || async move {
            runs1.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            42
        }).await
    });

    let runs2 = Arc::clone(&runs);
    let sf2 = sf.clone();
    let second = tokio::spawn(async move {
        sf2.run("a.ts", || async move {
            runs2.fetch_add(1, Ordering::SeqCst);
            7
        }).await
    });

    let (r1, r2) = tokio::join!(first, second);
    assert_eq!(r1.unwrap(), 42);
    assert_eq!(r2.unwrap(), 42);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_single_flight_leader_abort_cleans_map_and_permits_reuse() {
    let sf = Arc::new(SingleFlight::<i32>::new());

    // Task 1: starts work as leader but is aborted before completion
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let sf_clone = Arc::clone(&sf);
    let handle = tokio::spawn(async move {
        sf_clone.run("target.ts", || async move {
            let _ = started_tx.send(());
            // Await a signal that never completes until cancelled
            let pending: std::future::Pending<()> = std::future::pending();
            pending.await;
            100
        }).await
    });

    // Wait until leader has started and registered the flight
    started_rx.await.unwrap();

    // Abort the leader task
    handle.abort();
    let _ = handle.await; // Observe abort

    // Task 2: calling run on the same key must NOT panic or hang on closed channel;
    // RAII leader guard removed the stale key so Task 2 becomes the new leader and finishes!
    let res = sf.run("target.ts", || async {
        200
    }).await;

    assert_eq!(res, 200);
}

#[test]
fn test_session_state_notice_tracking_and_reset_semantics() {
    let state = create_post_mutation_session_state();
    assert!(state.should_notice("missing:biome", Some("s1")));
    assert!(!state.should_notice("missing:biome", Some("s1")));
    assert!(state.should_notice("missing:biome", Some("s2")));

    // reset(None) and reset(Some("")) are no-ops matching upstream
    state.reset(None);
    assert!(!state.should_notice("missing:biome", Some("s1")));
    state.reset(Some(""));
    assert!(!state.should_notice("missing:biome", Some("s1")));

    // reset(Some("s1")) resets only s1
    state.reset(Some("s1"));
    assert!(state.should_notice("missing:biome", Some("s1")));
    assert!(!state.should_notice("missing:biome", Some("s2")));
}
