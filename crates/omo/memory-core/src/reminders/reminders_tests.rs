use pretty_assertions::assert_eq;

use super::*;

#[test]
fn test_generate_reminders_when_clean_repo_then_returns_no_reminders() {
    let snapshot = RepoStatusSnapshot {
        dirty_paths: Vec::new(),
        conflict_state: ConflictState::None,
        ahead_count: 0,
        last_push_failed: None,
    };
    let mut state = create_reminders_state();
    let reminders = generate_reminders(&mut state, &snapshot);

    assert_eq!(reminders, Vec::<Reminder>::new());
}

#[test]
fn test_reminder_kind_for_when_clean_repo_then_null() {
    let snapshot = RepoStatusSnapshot {
        dirty_paths: Vec::new(),
        conflict_state: ConflictState::None,
        ahead_count: 0,
        last_push_failed: None,
    };
    assert_eq!(reminder_kind_for(&snapshot), None);
}

#[test]
fn test_generate_reminders_when_dirty_repo_then_emits_dirty_reminder_with_commit_needed_and_paths()
{
    let snapshot = RepoStatusSnapshot {
        dirty_paths: vec![
            "system/notes.md".to_string(),
            "reference/goals.md".to_string(),
        ],
        conflict_state: ConflictState::None,
        ahead_count: 0,
        last_push_failed: None,
    };
    let mut state = create_reminders_state();
    let reminders = generate_reminders(&mut state, &snapshot);

    assert_eq!(reminders.len(), 1);
    let reminder = &reminders[0];
    assert_eq!(reminder.kind, ReminderKind::Dirty);
    assert!(reminder.text.contains("MEMORY COMMIT NEEDED"));
    assert!(reminder.text.contains("system/notes.md"));
    assert!(reminder.text.contains("reference/goals.md"));
}

#[test]
fn test_generate_reminders_when_regenerating_same_snapshot_then_dedupes() {
    let snapshot = RepoStatusSnapshot {
        dirty_paths: vec![
            "system/notes.md".to_string(),
            "reference/goals.md".to_string(),
        ],
        conflict_state: ConflictState::None,
        ahead_count: 0,
        last_push_failed: None,
    };
    let mut state = create_reminders_state();
    let first = generate_reminders(&mut state, &snapshot);
    assert_eq!(first.len(), 1);

    let second = generate_reminders(&mut state, &snapshot);
    assert_eq!(second, Vec::<Reminder>::new());
}

#[test]
fn test_generate_reminders_when_cleared_to_clean_then_retriggering_emits_again() {
    let snapshot = RepoStatusSnapshot {
        dirty_paths: vec![
            "system/notes.md".to_string(),
            "reference/goals.md".to_string(),
        ],
        conflict_state: ConflictState::None,
        ahead_count: 0,
        last_push_failed: None,
    };
    let mut state = create_reminders_state();
    generate_reminders(&mut state, &snapshot);

    generate_reminders(
        &mut state,
        &RepoStatusSnapshot {
            dirty_paths: Vec::new(),
            conflict_state: ConflictState::None,
            ahead_count: 0,
            last_push_failed: None,
        },
    );

    let retriggered = generate_reminders(&mut state, &snapshot);
    assert_eq!(retriggered.len(), 1);
    assert_eq!(retriggered[0].kind, ReminderKind::Dirty);
}

#[test]
fn test_generate_reminders_when_merge_conflict_then_emits_conflict_reminder_with_resolution_guidance()
 {
    let snapshot = RepoStatusSnapshot {
        dirty_paths: Vec::new(),
        conflict_state: ConflictState::Merge,
        ahead_count: 0,
        last_push_failed: None,
    };
    let mut state = create_reminders_state();
    let reminders = generate_reminders(&mut state, &snapshot);

    assert_eq!(reminders.len(), 1);
    let reminder = &reminders[0];
    assert_eq!(reminder.kind, ReminderKind::Conflict);
    assert!(reminder.text.contains("MEMORY GIT CONFLICT"));
    assert!(reminder.text.contains("merge"));
    assert!(reminder.text.contains("resolve"));
}

#[test]
fn test_generate_reminders_when_conflict_persists_then_dedupes() {
    let snapshot = RepoStatusSnapshot {
        dirty_paths: Vec::new(),
        conflict_state: ConflictState::Merge,
        ahead_count: 0,
        last_push_failed: None,
    };
    let mut state = create_reminders_state();
    let first = generate_reminders(&mut state, &snapshot);
    let second = generate_reminders(&mut state, &snapshot);

    assert_eq!(first.len(), 1);
    assert_eq!(second, Vec::<Reminder>::new());
}

#[test]
fn test_generate_reminders_when_rebase_conflict_then_mentions_rebase() {
    let snapshot = RepoStatusSnapshot {
        dirty_paths: Vec::new(),
        conflict_state: ConflictState::Rebase,
        ahead_count: 0,
        last_push_failed: None,
    };
    let mut state = create_reminders_state();
    let reminders = generate_reminders(&mut state, &snapshot);

    assert_eq!(reminders.len(), 1);
    assert!(reminders[0].text.contains("rebase"));
}

#[test]
fn test_generate_reminders_when_unmerged_files_conflict_then_conflict_takes_priority_over_dirty() {
    let snapshot = RepoStatusSnapshot {
        dirty_paths: vec!["system/conflict.md".to_string()],
        conflict_state: ConflictState::Unmerged,
        ahead_count: 0,
        last_push_failed: None,
    };
    let mut state = create_reminders_state();
    let reminders = generate_reminders(&mut state, &snapshot);

    assert_eq!(reminders.len(), 1);
    assert_eq!(reminders[0].kind, ReminderKind::Conflict);
}

#[test]
fn test_generate_reminders_when_push_failed_then_emits_push_failed_reminder_with_reason() {
    let snapshot = RepoStatusSnapshot {
        dirty_paths: Vec::new(),
        conflict_state: ConflictState::None,
        ahead_count: 2,
        last_push_failed: Some(PushFailure {
            reason: "non-fast-forward".to_string(),
        }),
    };
    let mut state = create_reminders_state();
    let reminders = generate_reminders(&mut state, &snapshot);

    assert_eq!(reminders.len(), 1);
    let reminder = &reminders[0];
    assert_eq!(reminder.kind, ReminderKind::PushFailed);
    assert!(reminder.text.contains("MEMORY SYNC FAILED"));
    assert!(reminder.text.contains("non-fast-forward"));
}

#[test]
fn test_generate_reminders_when_push_remains_failed_then_dedupes() {
    let snapshot = RepoStatusSnapshot {
        dirty_paths: Vec::new(),
        conflict_state: ConflictState::None,
        ahead_count: 2,
        last_push_failed: Some(PushFailure {
            reason: "non-fast-forward".to_string(),
        }),
    };
    let mut state = create_reminders_state();
    let first = generate_reminders(&mut state, &snapshot);
    let second = generate_reminders(&mut state, &snapshot);

    assert_eq!(first.len(), 1);
    assert_eq!(second, Vec::<Reminder>::new());
}

#[test]
fn test_generate_reminders_when_different_failure_reason_then_re_emits() {
    let snapshot = RepoStatusSnapshot {
        dirty_paths: Vec::new(),
        conflict_state: ConflictState::None,
        ahead_count: 2,
        last_push_failed: Some(PushFailure {
            reason: "non-fast-forward".to_string(),
        }),
    };
    let mut state = create_reminders_state();
    generate_reminders(&mut state, &snapshot);

    let changed = generate_reminders(
        &mut state,
        &RepoStatusSnapshot {
            dirty_paths: Vec::new(),
            conflict_state: ConflictState::None,
            ahead_count: 2,
            last_push_failed: Some(PushFailure {
                reason: "auth denied".to_string(),
            }),
        },
    );
    assert_eq!(changed.len(), 1);
    assert!(changed[0].text.contains("auth denied"));
}

#[test]
fn test_generate_reminders_when_conflict_dirty_and_push_failed_simultaneously_then_conflict_takes_priority()
 {
    let snapshot = RepoStatusSnapshot {
        dirty_paths: vec!["a.md".to_string(), "b.md".to_string()],
        conflict_state: ConflictState::Merge,
        ahead_count: 1,
        last_push_failed: Some(PushFailure {
            reason: "network down".to_string(),
        }),
    };
    let mut state = create_reminders_state();
    let reminders = generate_reminders(&mut state, &snapshot);

    assert_eq!(reminders.len(), 1);
    assert_eq!(reminders[0].kind, ReminderKind::Conflict);
}

#[test]
fn test_generate_reminders_when_ahead_count_with_no_push_failure_then_no_reminder() {
    let snapshot = RepoStatusSnapshot {
        dirty_paths: Vec::new(),
        conflict_state: ConflictState::None,
        ahead_count: 3,
        last_push_failed: None,
    };
    let mut state = create_reminders_state();
    let reminders = generate_reminders(&mut state, &snapshot);

    assert_eq!(reminders, Vec::<Reminder>::new());
}

#[test]
fn test_generate_reminders_when_dirty_then_clean_then_different_paths_then_re_emits_after_clear() {
    let mut state = create_reminders_state();
    let first = generate_reminders(
        &mut state,
        &RepoStatusSnapshot {
            dirty_paths: vec!["a.md".to_string()],
            conflict_state: ConflictState::None,
            ahead_count: 0,
            last_push_failed: None,
        },
    );
    assert_eq!(first.len(), 1);

    generate_reminders(
        &mut state,
        &RepoStatusSnapshot {
            dirty_paths: Vec::new(),
            conflict_state: ConflictState::None,
            ahead_count: 0,
            last_push_failed: None,
        },
    );

    let retriggered = generate_reminders(
        &mut state,
        &RepoStatusSnapshot {
            dirty_paths: vec!["b.md".to_string(), "c.md".to_string()],
            conflict_state: ConflictState::None,
            ahead_count: 0,
            last_push_failed: None,
        },
    );
    assert_eq!(retriggered.len(), 1);
    assert!(retriggered[0].text.contains("b.md"));
}

#[test]
fn test_generate_reminders_when_state_persistence_across_calls_then_tracks_conditions() {
    let mut state = create_reminders_state();

    let r1 = generate_reminders(
        &mut state,
        &RepoStatusSnapshot {
            dirty_paths: vec!["x.md".to_string()],
            conflict_state: ConflictState::None,
            ahead_count: 0,
            last_push_failed: None,
        },
    );
    assert_eq!(r1.len(), 1);

    let r2 = generate_reminders(
        &mut state,
        &RepoStatusSnapshot {
            dirty_paths: vec!["x.md".to_string()],
            conflict_state: ConflictState::None,
            ahead_count: 0,
            last_push_failed: None,
        },
    );
    assert_eq!(r2.len(), 0);

    let r3 = generate_reminders(
        &mut state,
        &RepoStatusSnapshot {
            dirty_paths: Vec::new(),
            conflict_state: ConflictState::Merge,
            ahead_count: 0,
            last_push_failed: None,
        },
    );
    assert_eq!(r3.len(), 1);

    let r4 = generate_reminders(
        &mut state,
        &RepoStatusSnapshot {
            dirty_paths: Vec::new(),
            conflict_state: ConflictState::Merge,
            ahead_count: 0,
            last_push_failed: None,
        },
    );
    assert_eq!(r4.len(), 0);

    let r5 = generate_reminders(
        &mut state,
        &RepoStatusSnapshot {
            dirty_paths: Vec::new(),
            conflict_state: ConflictState::None,
            ahead_count: 0,
            last_push_failed: None,
        },
    );
    assert_eq!(r5.len(), 0);

    let r6 = generate_reminders(
        &mut state,
        &RepoStatusSnapshot {
            dirty_paths: vec!["y.md".to_string()],
            conflict_state: ConflictState::None,
            ahead_count: 0,
            last_push_failed: None,
        },
    );
    assert_eq!(r6.len(), 1);
    assert_eq!(r6[0].kind, ReminderKind::Dirty);
}

#[test]
fn test_reminder_kind_when_inspected_then_values_are_expected_union() {
    let mut state = create_reminders_state();
    let dirty = generate_reminders(
        &mut state,
        &RepoStatusSnapshot {
            dirty_paths: vec!["a".to_string()],
            conflict_state: ConflictState::None,
            ahead_count: 0,
            last_push_failed: None,
        },
    );
    let conflict = generate_reminders(
        &mut state,
        &RepoStatusSnapshot {
            dirty_paths: Vec::new(),
            conflict_state: ConflictState::Merge,
            ahead_count: 0,
            last_push_failed: None,
        },
    );
    generate_reminders(
        &mut state,
        &RepoStatusSnapshot {
            dirty_paths: Vec::new(),
            conflict_state: ConflictState::None,
            ahead_count: 0,
            last_push_failed: None,
        },
    );
    let push_failed = generate_reminders(
        &mut state,
        &RepoStatusSnapshot {
            dirty_paths: Vec::new(),
            conflict_state: ConflictState::None,
            ahead_count: 1,
            last_push_failed: Some(PushFailure {
                reason: "timeout".to_string(),
            }),
        },
    );

    assert_eq!(dirty[0].kind, ReminderKind::Dirty);
    assert_eq!(conflict[0].kind, ReminderKind::Conflict);
    assert_eq!(push_failed[0].kind, ReminderKind::PushFailed);
}
