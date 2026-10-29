//! Memory git status reminders: pure generator with condition-key dedupe.

use serde::{Deserialize, Serialize};

/// Conflict operation kind derived from git state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictState {
    None,
    Merge,
    Rebase,
    Unmerged,
}

/// Reason for a failed push, if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushFailure {
    pub reason: String,
}

/// Immutable snapshot of repository status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoStatusSnapshot {
    pub dirty_paths: Vec<String>,
    pub conflict_state: ConflictState,
    pub ahead_count: usize,
    pub last_push_failed: Option<PushFailure>,
}

/// Stable machine key identifying a reminder category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderKind {
    Dirty,
    Conflict,
    #[serde(rename = "push_failed")]
    PushFailed,
}

/// A single emitted reminder with category kind and rendered text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reminder {
    pub kind: ReminderKind,
    pub text: String,
}

/// Mutable dedupe state tracking the currently emitted condition key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemindersState {
    pub emitted_key: Option<String>,
}

/// Factory: create a fresh reminders state with nothing emitted.
pub fn create_reminders_state() -> RemindersState {
    RemindersState { emitted_key: None }
}

/// Determine the active reminder kind for a snapshot, or None when clean.
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

fn condition_key_for(snapshot: &RepoStatusSnapshot, kind: Option<ReminderKind>) -> Option<String> {
    let kind = kind?;
    match kind {
        ReminderKind::Dirty => Some("dirty".to_string()),
        ReminderKind::Conflict => {
            let state_str = match snapshot.conflict_state {
                ConflictState::None => "none",
                ConflictState::Merge => "merge",
                ConflictState::Rebase => "rebase",
                ConflictState::Unmerged => "unmerged",
            };
            Some(format!("conflict:{state_str}"))
        }
        ReminderKind::PushFailed => {
            let reason = snapshot
                .last_push_failed
                .as_ref()
                .map(|f| f.reason.as_str())
                .unwrap_or("");
            Some(format!("push_failed:{reason}"))
        }
    }
}

fn render_dirty_reminder(snapshot: &RepoStatusSnapshot) -> String {
    let paths = &snapshot.dirty_paths;
    let take_count = paths.len().min(20);
    let mut listing = paths[..take_count].join("\n  - ");
    if paths.len() > 20 {
        listing.push_str(&format!("\n  - ...and {} more", paths.len() - 20));
    }

    let parts = [
        "MEMORY COMMIT NEEDED: The memory repository has uncommitted changes.",
        "",
        "Uncommitted path(s):",
        &format!("  - {listing}"),
        "",
        "Commit these memory changes when appropriate. Do not run `git push` for MemFS sync; the harness pushes clean committed memory changes automatically for remote MemFS agents after turns.",
    ];
    parts.join("\n")
}

fn render_conflict_reminder(snapshot: &RepoStatusSnapshot) -> String {
    let op = snapshot.conflict_state;
    let op_label = match op {
        ConflictState::Merge => "merge in progress",
        ConflictState::Rebase => "rebase in progress",
        ConflictState::Unmerged | ConflictState::None => "unmerged files",
    };
    let unmerged_paths = &snapshot.dirty_paths;
    let paths_line = if !unmerged_paths.is_empty() {
        let take_count = unmerged_paths.len().min(20);
        let mut list = unmerged_paths[..take_count].join("\n  - ");
        if unmerged_paths.len() > 20 {
            list.push_str(&format!("\n  - ...and {} more", unmerged_paths.len() - 20));
        }
        format!("\n\nConflicted file(s):\n  - {list}")
    } else {
        String::new()
    };
    let complete_op = if op == ConflictState::Rebase {
        "rebase"
    } else {
        "merge"
    };

    let parts = [
        "MEMORY GIT CONFLICT: The memory repository needs manual conflict resolution.",
        "",
        &format!("Status: {op_label}."),
        &paths_line,
        "",
        &format!(
            "Resolve the conflicts in the memory repository, stage the resolved files, and complete the {complete_op}. The harness will retry remote push after a future turn when the repo is clean."
        ),
    ];
    parts.join("\n")
}

fn render_push_failed_reminder(snapshot: &RepoStatusSnapshot) -> String {
    let reason = snapshot
        .last_push_failed
        .as_ref()
        .map(|f| f.reason.as_str())
        .unwrap_or("unknown");
    let ahead = snapshot.ahead_count;
    let ahead_line = if ahead > 0 {
        format!("\nLocal commits ahead of remote: {ahead}.")
    } else {
        String::new()
    };

    let parts = [
        "MEMORY SYNC FAILED: The harness could not push pending memory commits.",
        "",
        &format!("Reason: {reason}.{ahead_line}"),
        "",
        "Inspect the memory repository and resolve any local git issue. The harness will retry remote push after a future turn when the repo is clean.",
    ];
    parts.join("\n")
}

fn render_reminder(snapshot: &RepoStatusSnapshot, kind: ReminderKind) -> String {
    match kind {
        ReminderKind::Dirty => render_dirty_reminder(snapshot),
        ReminderKind::Conflict => render_conflict_reminder(snapshot),
        ReminderKind::PushFailed => render_push_failed_reminder(snapshot),
    }
}

/// Generate reminders for a snapshot, applying condition-key dedupe against state.
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
    let text = render_reminder(snapshot, kind);
    vec![Reminder { kind, text }]
}

#[cfg(test)]
#[path = "reminders_tests.rs"]
mod tests;
