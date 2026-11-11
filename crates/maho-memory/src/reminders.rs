//! The one-line nudge telling the agent its memory repository needs attention.
//!
//! Spec: `docs/cleanroom/specs/memory-reminders.md`. A reminder is shown once per *condition*, not once per
//! turn: a store left dirty for twenty turns must not print twenty times, because the message then becomes
//! noise the reader learns to skip.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictState {
    None,
    Merge,
    Rebase,
    Unmerged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushFailure {
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoStatusSnapshot {
    pub dirty_paths: Vec<String>,
    pub conflict_state: ConflictState,
    pub ahead_count: usize,
    pub last_push_failed: Option<PushFailure>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderKind {
    Dirty,
    Conflict,
    #[serde(rename = "push_failed")]
    PushFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reminder {
    pub kind: ReminderKind,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemindersState {
    pub emitted_key: Option<String>,
}

pub fn create_reminders_state() -> RemindersState {
    RemindersState::default()
}

/// The most urgent condition, or `None` when the store is clean.
///
/// A conflict outranks a failed push, which outranks uncommitted work: a conflict is the state where further
/// memory writes would stack on top of an unresolved merge, and it has to be named first.
pub fn reminder_kind_for(snapshot: &RepoStatusSnapshot) -> Option<ReminderKind> {
    if snapshot.conflict_state != ConflictState::None {
        return Some(ReminderKind::Conflict);
    }
    if snapshot.last_push_failed.is_some() {
        return Some(ReminderKind::PushFailed);
    }
    if !snapshot.dirty_paths.is_empty() {
        return Some(ReminderKind::Dirty);
    }
    None
}

/// What identifies the condition, so repeats of it are suppressed.
///
/// A dirty store keys on `dirty` alone: any number of edits stays one condition. A conflict and a failed
/// push key on their *state*, so a rebase becoming a merge, or a push failing for a new reason, is a new
/// thing worth saying.
fn condition_key_for(snapshot: &RepoStatusSnapshot, kind: Option<ReminderKind>) -> Option<String> {
    let kind = kind?;
    match kind {
        ReminderKind::Dirty => Some("dirty".to_owned()),
        ReminderKind::Conflict => {
            let state = match snapshot.conflict_state {
                ConflictState::None => "none",
                ConflictState::Merge => "merge",
                ConflictState::Rebase => "rebase",
                ConflictState::Unmerged => "unmerged",
            };
            Some(format!("conflict:{state}"))
        }
        ReminderKind::PushFailed => {
            let reason = snapshot
                .last_push_failed
                .as_ref()
                .map(|failure| failure.reason.as_str())
                .unwrap_or("");
            Some(format!("push_failed:{reason}"))
        }
    }
}

const LIST_LIMIT: usize = 20;

fn path_listing(paths: &[String]) -> String {
    let shown = paths.len().min(LIST_LIMIT);
    let mut listing = paths[..shown].join("\n  - ");
    if paths.len() > LIST_LIMIT {
        listing.push_str(&format!("\n  - ...and {} more", paths.len() - LIST_LIMIT));
    }
    listing
}

fn render_dirty_reminder(snapshot: &RepoStatusSnapshot) -> String {
    [
        "MEMORY COMMIT NEEDED: The memory repository has uncommitted changes.",
        "",
        "Uncommitted path(s):",
        &format!("  - {}", path_listing(&snapshot.dirty_paths)),
        "",
        "Commit these memory changes when appropriate. Do not run `git push` for MemFS sync; the harness pushes clean committed memory changes automatically for remote MemFS agents after turns.",
    ]
    .join("\n")
}

fn render_conflict_reminder(snapshot: &RepoStatusSnapshot) -> String {
    let operation = snapshot.conflict_state;
    let label = match operation {
        ConflictState::Merge => "merge in progress",
        ConflictState::Rebase => "rebase in progress",
        ConflictState::Unmerged | ConflictState::None => "unmerged files",
    };
    let paths_line = if snapshot.dirty_paths.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nConflicted file(s):\n  - {}",
            path_listing(&snapshot.dirty_paths)
        )
    };
    let completion = if operation == ConflictState::Rebase {
        "rebase"
    } else {
        "merge"
    };
    [
        "MEMORY GIT CONFLICT: The memory repository needs manual conflict resolution.",
        "",
        &format!("Status: {label}."),
        &paths_line,
        "",
        &format!(
            "Resolve the conflicts in the memory repository, stage the resolved files, and complete the {completion}. The harness will retry remote push after a future turn when the repo is clean."
        ),
    ]
    .join("\n")
}

fn render_push_failed_reminder(snapshot: &RepoStatusSnapshot) -> String {
    let reason = snapshot
        .last_push_failed
        .as_ref()
        .map(|failure| failure.reason.as_str())
        .unwrap_or("unknown");
    let ahead_line = if snapshot.ahead_count > 0 {
        format!("\nLocal commits ahead of remote: {}.", snapshot.ahead_count)
    } else {
        String::new()
    };
    [
        "MEMORY SYNC FAILED: The harness could not push pending memory commits.",
        "",
        &format!("Reason: {reason}.{ahead_line}"),
        "",
        "Inspect the memory repository and resolve any local git issue. The harness will retry remote push after a future turn when the repo is clean.",
    ]
    .join("\n")
}

fn render_reminder(snapshot: &RepoStatusSnapshot, kind: ReminderKind) -> String {
    match kind {
        ReminderKind::Dirty => render_dirty_reminder(snapshot),
        ReminderKind::Conflict => render_conflict_reminder(snapshot),
        ReminderKind::PushFailed => render_push_failed_reminder(snapshot),
    }
}

/// Reminders for this snapshot, suppressing a condition already reported.
///
/// Returning to a clean store clears the key, so a condition that clears and comes back is reported again.
pub fn generate_reminders(
    state: &mut RemindersState,
    snapshot: &RepoStatusSnapshot,
) -> Vec<Reminder> {
    let kind = reminder_kind_for(snapshot);
    let key = condition_key_for(snapshot, kind);
    let Some(kind) = kind else {
        state.emitted_key = None;
        return Vec::new();
    };
    if state.emitted_key == key {
        return Vec::new();
    }
    state.emitted_key = key;
    vec![Reminder {
        kind,
        text: render_reminder(snapshot, kind),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean() -> RepoStatusSnapshot {
        RepoStatusSnapshot {
            dirty_paths: Vec::new(),
            conflict_state: ConflictState::None,
            ahead_count: 0,
            last_push_failed: None,
        }
    }

    fn dirty(paths: &[&str]) -> RepoStatusSnapshot {
        RepoStatusSnapshot {
            dirty_paths: paths.iter().map(|path| (*path).to_owned()).collect(),
            ..clean()
        }
    }

    #[test]
    fn a_clean_store_has_no_reminder() {
        assert_eq!(reminder_kind_for(&clean()), None);
        let mut state = create_reminders_state();
        assert!(generate_reminders(&mut state, &clean()).is_empty());
    }

    #[test]
    fn uncommitted_paths_raise_a_dirty_reminder() {
        assert_eq!(
            reminder_kind_for(&dirty(&["system/human.md"])),
            Some(ReminderKind::Dirty)
        );
    }

    #[test]
    fn a_conflict_outranks_uncommitted_paths() {
        let snapshot = RepoStatusSnapshot {
            conflict_state: ConflictState::Merge,
            ..dirty(&["system/human.md"])
        };
        assert_eq!(reminder_kind_for(&snapshot), Some(ReminderKind::Conflict));
    }

    #[test]
    fn a_failed_push_outranks_uncommitted_paths() {
        let snapshot = RepoStatusSnapshot {
            last_push_failed: Some(PushFailure {
                reason: "rejected".to_owned(),
            }),
            ..dirty(&["system/human.md"])
        };
        assert_eq!(reminder_kind_for(&snapshot), Some(ReminderKind::PushFailed));
    }

    #[test]
    fn a_conflict_outranks_a_failed_push() {
        let snapshot = RepoStatusSnapshot {
            conflict_state: ConflictState::Rebase,
            last_push_failed: Some(PushFailure {
                reason: "rejected".to_owned(),
            }),
            ..clean()
        };
        assert_eq!(reminder_kind_for(&snapshot), Some(ReminderKind::Conflict));
    }

    #[test]
    fn the_same_condition_is_reported_once() {
        let mut state = create_reminders_state();
        let snapshot = dirty(&["a.md"]);
        assert_eq!(generate_reminders(&mut state, &snapshot).len(), 1);
        assert!(generate_reminders(&mut state, &snapshot).is_empty());
        assert!(generate_reminders(&mut state, &dirty(&["b.md"])).is_empty());
    }

    #[test]
    fn a_condition_that_clears_and_returns_is_reported_again() {
        let mut state = create_reminders_state();
        let snapshot = dirty(&["a.md"]);
        assert_eq!(generate_reminders(&mut state, &snapshot).len(), 1);
        assert!(generate_reminders(&mut state, &clean()).is_empty());
        assert_eq!(generate_reminders(&mut state, &snapshot).len(), 1);
    }

    #[test]
    fn a_changed_conflict_state_is_a_new_condition() {
        let mut state = create_reminders_state();
        let merge = RepoStatusSnapshot {
            conflict_state: ConflictState::Merge,
            ..clean()
        };
        let rebase = RepoStatusSnapshot {
            conflict_state: ConflictState::Rebase,
            ..clean()
        };
        assert_eq!(generate_reminders(&mut state, &merge).len(), 1);
        assert_eq!(generate_reminders(&mut state, &rebase).len(), 1);
    }

    #[test]
    fn a_different_push_failure_is_a_new_condition() {
        let mut state = create_reminders_state();
        let first = RepoStatusSnapshot {
            last_push_failed: Some(PushFailure {
                reason: "rejected".to_owned(),
            }),
            ..clean()
        };
        let second = RepoStatusSnapshot {
            last_push_failed: Some(PushFailure {
                reason: "auth".to_owned(),
            }),
            ..clean()
        };
        assert_eq!(generate_reminders(&mut state, &first).len(), 1);
        assert_eq!(generate_reminders(&mut state, &second).len(), 1);
    }

    #[test]
    fn the_dirty_reminder_lists_the_paths() {
        let reminder = generate_reminders(
            &mut create_reminders_state(),
            &dirty(&["system/human.md", "notes/a.md"]),
        );
        assert_eq!(reminder[0].kind, ReminderKind::Dirty);
        assert!(reminder[0].text.contains("system/human.md"));
        assert!(reminder[0].text.contains("notes/a.md"));
    }

    #[test]
    fn a_long_path_list_is_truncated_with_a_count() {
        let paths: Vec<String> = (0..25).map(|index| format!("notes/{index}.md")).collect();
        let snapshot = RepoStatusSnapshot {
            dirty_paths: paths,
            ..clean()
        };
        let reminder = generate_reminders(&mut create_reminders_state(), &snapshot);
        assert!(reminder[0].text.contains("...and 5 more"));
        assert!(!reminder[0].text.contains("notes/24.md"));
    }

    #[test]
    fn the_dirty_reminder_tells_the_reader_not_to_push() {
        let reminder = generate_reminders(&mut create_reminders_state(), &dirty(&["a.md"]));
        assert!(reminder[0].text.contains("Do not run `git push`"));
    }

    #[test]
    fn a_rebase_conflict_asks_for_a_rebase_to_be_completed() {
        let snapshot = RepoStatusSnapshot {
            conflict_state: ConflictState::Rebase,
            ..clean()
        };
        let reminder = generate_reminders(&mut create_reminders_state(), &snapshot);
        assert!(reminder[0].text.contains("rebase in progress"));
        assert!(reminder[0].text.contains("complete the rebase"));
    }

    #[test]
    fn a_merge_conflict_asks_for_a_merge_to_be_completed() {
        let snapshot = RepoStatusSnapshot {
            conflict_state: ConflictState::Merge,
            ..clean()
        };
        let reminder = generate_reminders(&mut create_reminders_state(), &snapshot);
        assert!(reminder[0].text.contains("complete the merge"));
    }

    #[test]
    fn unmerged_without_a_known_operation_still_reads_as_unmerged() {
        let snapshot = RepoStatusSnapshot {
            conflict_state: ConflictState::Unmerged,
            ..clean()
        };
        let reminder = generate_reminders(&mut create_reminders_state(), &snapshot);
        assert!(reminder[0].text.contains("unmerged files"));
    }

    #[test]
    fn the_conflict_reminder_names_the_conflicted_files() {
        let snapshot = RepoStatusSnapshot {
            conflict_state: ConflictState::Merge,
            ..dirty(&["system/human.md"])
        };
        let reminder = generate_reminders(&mut create_reminders_state(), &snapshot);
        assert!(reminder[0].text.contains("Conflicted file(s)"));
        assert!(reminder[0].text.contains("system/human.md"));
    }

    #[test]
    fn the_push_failure_reminder_carries_the_reason() {
        let snapshot = RepoStatusSnapshot {
            last_push_failed: Some(PushFailure {
                reason: "non-fast-forward".to_owned(),
            }),
            ..clean()
        };
        let reminder = generate_reminders(&mut create_reminders_state(), &snapshot);
        assert!(reminder[0].text.contains("non-fast-forward"));
    }

    #[test]
    fn the_push_failure_reminder_counts_commits_ahead() {
        let snapshot = RepoStatusSnapshot {
            ahead_count: 3,
            last_push_failed: Some(PushFailure {
                reason: "rejected".to_owned(),
            }),
            ..clean()
        };
        let reminder = generate_reminders(&mut create_reminders_state(), &snapshot);
        assert!(reminder[0].text.contains("ahead of remote: 3"));
    }

    #[test]
    fn no_commits_ahead_omits_the_line() {
        let snapshot = RepoStatusSnapshot {
            last_push_failed: Some(PushFailure {
                reason: "rejected".to_owned(),
            }),
            ..clean()
        };
        let reminder = generate_reminders(&mut create_reminders_state(), &snapshot);
        assert!(!reminder[0].text.contains("ahead of remote"));
    }
}
