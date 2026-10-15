use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use pretty_assertions::assert_eq;

use super::kill_point::{maybe_kill_at, MEMORY_KILL_POINTS};
use super::receipts::{
    append_memory_receipt, append_memory_receipt_once, read_memory_receipts, receipt_identity,
    receipts_path, MemoryReceiptIdentityInput, MemoryReceiptInput, ReadMemoryReceiptsOptions,
};

fn lines(dir: &Path) -> Vec<String> {
    let path = receipts_path(dir);
    if !path.exists() {
        return Vec::new();
    }
    let text = std::fs::read_to_string(path).unwrap();
    text.split('\n')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

#[test]
fn given_a_launched_dream_when_appended_then_one_private_line_records_who_what_and_where() {
    // given
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();

    // when
    append_memory_receipt(
        dir,
        &MemoryReceiptInput::Run {
            kind: "dream".to_string(),
            run_id: "run-1".to_string(),
            trigger: "idle".to_string(),
            event: "launched".to_string(),
            generation: "2026-01-01T00:00:00.000Z".to_string(),
            sha: None,
            reason: None,
            detail: None,
        },
    )
    .unwrap();

    // then
    let written = lines(dir);
    assert_eq!(written.len(), 1);
    let receipt: serde_json::Value = serde_json::from_str(&written[0]).unwrap();
    assert_eq!(receipt["v"], 1);
    assert_eq!(receipt["kind"], "dream");
    assert_eq!(receipt["runId"], "run-1");
    assert_eq!(receipt["trigger"], "idle");
    assert_eq!(receipt["event"], "launched");
    assert_eq!(receipt["pid"], std::process::id());
    assert!(receipt["host"].is_string());
    assert!(crate::support::time::parse_rfc3339(receipt["at"].as_str().unwrap()).is_some());

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mode = std::fs::metadata(receipts_path(dir)).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn given_appends_racing_in_one_process_and_three_threads_when_they_finish_then_every_line_is_whole()
{
    // given
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().to_path_buf();

    // when: 20 main-thread appends + 3 threads with 10 appends each = 50
    let mut handles = Vec::new();
    for child_id in ["child-a", "child-b", "child-c"] {
        let child_dir = dir.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..10 {
                append_memory_receipt(
                    &child_dir,
                    &MemoryReceiptInput::Run {
                        kind: "dream".to_string(),
                        run_id: format!("{child_id}-{i}"),
                        trigger: "idle".to_string(),
                        event: "launched".to_string(),
                        generation: "g1".to_string(),
                        sha: None,
                        reason: None,
                        detail: None,
                    },
                )
                .unwrap();
            }
        }));
    }

    for i in 0..20 {
        append_memory_receipt(
            &dir,
            &MemoryReceiptInput::Run {
                kind: "dream".to_string(),
                run_id: format!("local-{i}"),
                trigger: "idle".to_string(),
                event: "launched".to_string(),
                generation: "g1".to_string(),
                sha: None,
                reason: None,
                detail: None,
            },
        )
        .unwrap();
    }

    for h in handles {
        h.join().unwrap();
    }

    // then
    let written = lines(&dir);
    assert_eq!(written.len(), 50);
    for line in &written {
        let val: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(val["event"], "launched");
    }
    let unique_runs: std::collections::HashSet<String> = written
        .iter()
        .map(|line| {
            let val: serde_json::Value = serde_json::from_str(line).unwrap();
            val["runId"].as_str().unwrap().to_string()
        })
        .collect();
    assert_eq!(unique_runs.len(), 50);
}

#[test]
fn given_a_long_or_secret_bearing_detail_when_appended_then_it_is_cut_to_400_characters_and_masked()
{
    // given
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();
    let detail = format!("push token=abc123456 {}", "x".repeat(600));

    // when
    append_memory_receipt(
        dir,
        &MemoryReceiptInput::Facts {
            kind: "facts".to_string(),
            batch_id: "b-1".to_string(),
            trigger: "settle".to_string(),
            event: "failed".to_string(),
            sha: None,
            reason: None,
            detail: Some(detail),
        },
    )
    .unwrap();

    // then
    let written = lines(dir);
    assert_eq!(written.len(), 1);
    let receipt: serde_json::Value = serde_json::from_str(&written[0]).unwrap();
    let det = receipt["detail"].as_str().unwrap();
    assert!(!det.contains("abc123456"));
    assert!(det.contains("***"));
    assert!(det.len() <= 400);
    assert!(det.ends_with("..."));
}

#[test]
fn append_once_ignores_unparseable_lines_with_a_matching_identity() {
    let root = tempfile::tempdir().unwrap();
    // Same identity fields, but not a valid v1 receipt (read_memory_receipts skips both lines).
    let stale = [
        serde_json::json!({"v":2,"at":"x","event":"committed","kind":"facts","batchId":"b"}),
        serde_json::json!({"v":1,"event":"committed","kind":"facts","batchId":"b"}),
    ];
    std::fs::write(receipts_path(root.path()), stale.iter().map(|line| format!("{line}\n")).collect::<String>()).unwrap();
    let input = MemoryReceiptInput::Facts { kind:"facts".into(), batch_id:"b".into(), trigger:"settle".into(), event:"committed".into(), sha:None, reason:None, detail:None };
    assert!(append_memory_receipt_once(root.path(), &input).unwrap());
    assert!(!append_memory_receipt_once(root.path(), &input).unwrap());
    assert_eq!(read_memory_receipts(root.path(), &ReadMemoryReceiptsOptions::default()).unwrap().receipts.len(), 1);
}

#[test]
fn loose_receipt_metadata_is_accepted_and_preserved_without_defaults() {
    let root = tempfile::tempdir().unwrap();
    let raw = serde_json::json!({"v":1,"at":"","event":"","kind":"facts","batchId":"loose","host":false,"pid":4294967297_u64,"extra":{"retained":true}});
    std::fs::write(receipts_path(root.path()), format!("{raw}\n")).unwrap();
    let result = read_memory_receipts(root.path(), &ReadMemoryReceiptsOptions::default()).unwrap();
    assert_eq!(result.skipped_partial_lines, 0);
    assert_eq!(serde_json::to_value(&result.receipts[0]).unwrap(), raw);
    assert!(!append_memory_receipt_once(root.path(), &MemoryReceiptInput::Facts { kind:"facts".into(), batch_id:"loose".into(), trigger:"settle".into(), event:"".into(), sha:None, reason:None, detail:None }).unwrap());
}

#[test]
fn receipt_reason_and_detail_bound_utf16_without_splitting_utf8() {
    let root = tempfile::tempdir().unwrap();
    let input = MemoryReceiptInput::Facts {
        kind: "facts".into(), batch_id: "unicode".into(), trigger: "settle".into(), event: "failed".into(), sha: None,
        reason: Some(format!("token=abc123456 {}", "\u{ac00}".repeat(600))),
        detail: Some("\u{ac00}".repeat(600)),
    };
    append_memory_receipt(root.path(), &input).unwrap();
    let value: serde_json::Value = serde_json::from_str(&lines(root.path())[0]).unwrap();
    assert_eq!(value["detail"], format!("{}...", "\u{ac00}".repeat(397)));
    let reason = value["reason"].as_str().unwrap();
    assert!(!reason.contains("abc123456"));
    assert_eq!(reason.encode_utf16().count(), 400);
}

#[test]
fn given_mixed_kinds_and_a_line_cut_off_by_a_crash_when_read_then_the_newest_of_one_kind_return_and_the_partial_line_is_counted(
) {
    // given
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();
    for index in 0..7 {
        append_memory_receipt(
            dir,
            &MemoryReceiptInput::Run {
                kind: "dream".to_string(),
                run_id: format!("d-{index}"),
                trigger: "idle".to_string(),
                event: "merged".to_string(),
                generation: format!("g{index}"),
                sha: None,
                reason: None,
                detail: None,
            },
        )
        .unwrap();
        append_memory_receipt(
            dir,
            &MemoryReceiptInput::Run {
                kind: "reflection".to_string(),
                run_id: format!("r-{index}"),
                trigger: "step".to_string(),
                event: "no_changes".to_string(),
                generation: format!("g{index}"),
                sha: None,
                reason: None,
                detail: None,
            },
        )
        .unwrap();
    }

    // append partial line
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(receipts_path(dir))
        .unwrap();
    file.write_all(b"{\"v\":1,\"kind\":\"dream\",\"runId\":\"cut")
        .unwrap();
    drop(file);

    // when
    let read = read_memory_receipts(
        dir,
        &ReadMemoryReceiptsOptions {
            kind: Some("dream"),
            limit: Some(5),
        },
    )
    .unwrap();

    // then
    let ids: Vec<String> = read
        .receipts
        .iter()
        .map(|r| match &r.input {
            MemoryReceiptInput::Run { run_id, .. } => run_id.clone(),
            MemoryReceiptInput::Facts { batch_id, .. } => batch_id.clone(),
        })
        .collect();
    assert_eq!(ids, vec!["d-6", "d-5", "d-4", "d-3", "d-2"]);
    assert_eq!(read.skipped_partial_lines, 1);
}

#[test]
fn given_no_receipts_file_when_read_then_nothing_is_returned_and_nothing_is_skipped() {
    // given
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();

    // when
    let read = read_memory_receipts(dir, &ReadMemoryReceiptsOptions::default()).unwrap();

    // then
    assert_eq!(read.receipts, Vec::new());
    assert_eq!(read.skipped_partial_lines, 0);
}

#[test]
fn given_the_same_merge_reported_from_four_threads_when_appended_once_then_exactly_one_line_exists_and_a_different_event_still_lands(
) {
    // given
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().to_path_buf();
    let merged = MemoryReceiptInput::Run {
        kind: "dream".to_string(),
        run_id: "same".to_string(),
        trigger: "idle".to_string(),
        event: "merged".to_string(),
        generation: "g1".to_string(),
        sha: None,
        reason: None,
        detail: None,
    };

    // when
    let mut handles = Vec::new();
    for _ in 0..3 {
        let child_dir = dir.clone();
        let child_merged = merged.clone();
        handles.push(std::thread::spawn(move || {
            for _ in 0..5 {
                append_memory_receipt_once(&child_dir, &child_merged).unwrap();
            }
        }));
    }
    for _ in 0..10 {
        append_memory_receipt_once(&dir, &merged).unwrap();
    }
    for h in handles {
        h.join().unwrap();
    }

    let launched_input = MemoryReceiptInput::Run {
        kind: "dream".to_string(),
        run_id: "same".to_string(),
        trigger: "idle".to_string(),
        event: "launched".to_string(),
        generation: "g1".to_string(),
        sha: None,
        reason: None,
        detail: None,
    };
    let launched = append_memory_receipt_once(&dir, &launched_input).unwrap();
    let again = append_memory_receipt_once(&dir, &merged).unwrap();

    // then
    let written = lines(&dir);
    let receipts: Vec<serde_json::Value> = written
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let merged_count = receipts
        .iter()
        .filter(|r| r["event"] == "merged")
        .count();
    let launched_count = receipts
        .iter()
        .filter(|r| r["event"] == "launched")
        .count();
    assert_eq!(merged_count, 1);
    assert_eq!(launched_count, 1);
    assert!(launched);
    assert!(!again);
}

#[test]
fn given_one_run_in_two_generations_when_identities_are_compared_then_generations_stay_apart_and_a_re_read_ledger_gives_the_same_key(
) {
    // given
    let run_id = "run-x";
    let started_at = "2026-01-01T00:00:00.000Z";

    // when
    let live = receipt_identity(&MemoryReceiptIdentityInput::Run {
        kind: "dream",
        run_id,
        event: "merged",
        generation: started_at,
    });
    let backfilled = receipt_identity(&MemoryReceiptIdentityInput::Run {
        kind: "dream",
        run_id,
        event: "merged",
        generation: started_at,
    });
    let retired = receipt_identity(&MemoryReceiptIdentityInput::Run {
        kind: "dream",
        run_id,
        event: "merged",
        generation: "2025-12-31T00:00:00.000Z",
    });
    let facts = receipt_identity(&MemoryReceiptIdentityInput::Facts {
        kind: "facts",
        batch_id: "batch-1",
        event: "committed",
    });

    // then
    assert_eq!(backfilled, live);
    assert_ne!(retired, live);
    assert_eq!(
        facts,
        receipt_identity(&MemoryReceiptIdentityInput::Facts {
            kind: "facts",
            batch_id: "batch-1",
            event: "committed"
        })
    );
}

#[test]
fn given_kill_points_when_the_variable_is_unset_different_or_matching_then_only_a_match_kills_this_process_with_the_platforms_signal(
) {
    // given
    type KillRecord = (u32, Option<String>);
    let kills: Arc<std::sync::Mutex<Vec<KillRecord>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let kills_clone = kills.clone();
    let kill = move |pid: u32, sig: Option<&str>| {
        kills_clone
            .lock()
            .unwrap()
            .push((pid, sig.map(str::to_string)));
    };

    // when
    let unset = maybe_kill_at(
        "after-merge",
        super::kill_point::KillPointOptions {
            kill: Some(&kill),
            platform: None,
            env_var: None,
        },
    );

    let other = maybe_kill_at(
        "after-merge",
        super::kill_point::KillPointOptions {
            kill: Some(&kill),
            platform: None,
            env_var: Some("after-validate"),
        },
    );

    let hit = maybe_kill_at(
        "after-merge",
        super::kill_point::KillPointOptions {
            kill: Some(&kill),
            platform: Some("linux"),
            env_var: Some("after-merge"),
        },
    );
    let hit_windows = maybe_kill_at(
        "after-merge",
        super::kill_point::KillPointOptions {
            kill: Some(&kill),
            platform: Some("win32"),
            env_var: Some("after-merge"),
        },
    );

    // then
    assert_eq!(unset, None);
    assert_eq!(other, None);
    assert_eq!(hit, Some("signal"));
    assert_eq!(hit_windows, Some("terminate"));

    let recorded = kills.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![
            (std::process::id(), Some("SIGKILL".to_string())),
            (std::process::id(), None),
        ]
    );
    assert_eq!(
        MEMORY_KILL_POINTS,
        [
            "after-reserve",
            "after-prelaunch",
            "after-worktree",
            "after-child-exit",
            "after-validate",
            "after-merge",
            "before-receipt",
        ]
    );
}

#[test]
fn given_a_fresh_runtime_dir_with_no_locks_dir_when_appended_then_the_receipts_lock_is_taken_under_runtime_locks_and_the_append_lands(
) {
    // given
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();

    // when
    append_memory_receipt(
        dir,
        &MemoryReceiptInput::Run {
            kind: "reflection".to_string(),
            run_id: "r".to_string(),
            trigger: "manual".to_string(),
            event: "launched".to_string(),
            generation: "g".to_string(),
            sha: None,
            reason: None,
            detail: None,
        },
    )
    .unwrap();

    // then
    assert!(dir.join("locks").is_dir());
    assert_eq!(lines(dir).len(), 1);
}
