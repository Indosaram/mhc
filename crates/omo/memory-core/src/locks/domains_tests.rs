use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;

use super::*;

#[test]
fn given_runtime_locks_dir_when_domain_paths_resolved_then_protocol_names_are_stable() {
    // #given
    let locks_directory = Path::new("runtime").join("locks");

    // #when
    let paths = [
        memory_writer_lock_path(&locks_directory),
        reflection_scheduler_lock_path(&locks_directory),
        skills_usage_lock_path(&locks_directory),
        transcript_state_lock_path(&locks_directory, "conversation/../one").unwrap(),
        facts_queue_lock_path(&locks_directory),
        facts_runs_lock_path(&locks_directory),
        notice_lock_path(&locks_directory),
        run_finalization_lock_path(&locks_directory, "run-123").unwrap(),
    ];

    // #then
    assert_eq!(
        LOCK_DOMAINS,
        [
            "memory-write",
            "reflection-scheduler",
            "reflection-finalize",
            "transcript-state",
            "skills-usage",
            "facts-queue",
            "facts-runs",
            "notice",
        ]
    );
    assert_eq!(paths[0], locks_directory.join("memory-write.lock"));
    assert_eq!(paths[1], locks_directory.join("reflection-scheduler.lock"));
    assert_eq!(paths[2], locks_directory.join("skills-usage.lock"));
    assert_eq!(paths[3].parent(), Some(locks_directory.as_path()));
    let transcript_name = paths[3].file_name().unwrap().to_str().unwrap();
    assert!(transcript_name.starts_with("transcript-state-"));
    assert!(transcript_name.ends_with(".lock"));
    let hex_part =
        &transcript_name["transcript-state-".len()..transcript_name.len() - ".lock".len()];
    assert_eq!(hex_part.len(), 16);
    assert!(hex_part.chars().all(|c| c.is_ascii_hexdigit()));

    assert_eq!(paths[4], locks_directory.join("facts-queue.lock"));
    assert_eq!(paths[5], locks_directory.join("facts-runs.lock"));
    assert_eq!(paths[6], locks_directory.join("notice.lock"));
    assert_eq!(paths[7], locks_directory.join("finalize-run-123.lock"));
}

#[test]
fn given_hostile_and_distinct_run_ids_when_resolved_then_confined_and_distinct() {
    // #given
    let locks_directory = PathBuf::from("runtime").join("locks");

    // #when
    let hostile = run_finalization_lock_path(&locks_directory, "../../outside/run").unwrap();
    let first = run_finalization_lock_path(&locks_directory, "run-one").unwrap();
    let second = run_finalization_lock_path(&locks_directory, "run-two").unwrap();

    // #then
    assert_eq!(hostile.parent(), Some(locks_directory.as_path()));
    let hostile_name = hostile.file_name().unwrap().to_str().unwrap();
    assert!(hostile_name.starts_with("finalize-"));
    assert!(hostile_name.ends_with(".lock"));
    assert!(hostile.starts_with(&locks_directory));
    assert_ne!(first, second);
    assert!(run_finalization_lock_path(&locks_directory, ".").is_err());
    assert!(run_finalization_lock_path(&locks_directory, "..").is_err());
    assert!(run_finalization_lock_path(&locks_directory, "   ").is_err());
    assert!(transcript_state_lock_path(&locks_directory, "").is_err());
}
