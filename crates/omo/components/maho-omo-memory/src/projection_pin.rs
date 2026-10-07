//! Session-pinned memory projection (pin `projection-pin.ts`).
//!
//! A session compiles its memory block at the memory HEAD of its first turn and keeps those bytes:
//! any later commit that reached the system prompt would change the prompt hash and cost a cache
//! write of the whole conversation behind it. What changed after the pin reaches the model as a
//! line of the late `<memory_notice>` message instead. The pin is a session entry, so a resume, a
//! host restart or a runtime reload reproduces the same bytes. It moves only where the prompt cache
//! is already cold or the user asked for it: after a compaction, on `/recompile`, or when a history
//! rewrite took the pinned commit away.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use maho_ext_api::SessionEntry;
use memory_core::compile::{
    ProjectedChanges, ProjectedChangesOptions, is_empty_projected_changes,
    projected_changes_between, revision_exists,
};
use memory_core::git::GitMemoryRepo;
use memory_core::git::errors::GitError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROJECTION_PIN_ENTRY_TYPE: &str = "omo-memory:projection-pin";
pub const MEMORY_CHANGES_METADATA_TOKEN: &str =
    "Memory changed after this session's prompt was pinned to";
const MAX_LISTED_PATHS: usize = 20;

/// The persisted pin, read back from a session entry on resume.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectionPinRecord {
    pub version: u32,
    pub session_id: String,
    pub revision: Option<String>,
    pub noticed_through: Option<String>,
    pub compaction_id: Option<String>,
    pub pinned_at_ms: f64,
}

/// Why a session stopped reusing its pinned revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionRepinReason {
    FirstTurn,
    Compaction,
    Refresh,
    MissingRevision,
}

/// The revision a turn compiles at, plus the changes it must announce.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectionTurn {
    pub revision: Option<String>,
    pub changes: Option<ProjectedChanges>,
    pub repinned: Option<ProjectionRepinReason>,
}

/// Everything `advance`/`peek` read for one turn.
pub struct ProjectionAdvanceInput<'a> {
    pub repo: &'a GitMemoryRepo,
    pub session_id: &'a str,
    pub branch: &'a [SessionEntry],
    pub head: Option<String>,
}

/// Port of the upstream `ProjectionPins` interface.
pub trait ProjectionPins {
    fn advance(
        &self,
        input: ProjectionAdvanceInput<'_>,
        record: &dyn Fn(&ProjectionPinRecord),
    ) -> Result<ProjectionTurn, GitError>;
    /// The revision the next `advance` compiles at, without recording, caching or announcing
    /// anything: a `before_agent_start` preview must compose the real turn's bytes and leave the
    /// session untouched.
    ///
    /// BLOCKED consumer: `maho_ext_api::BeforeAgentStartEvent` has no `preview` flag, so nothing
    /// native can call this yet. See the boundary report for the exact missing field.
    fn peek(&self, input: ProjectionAdvanceInput<'_>) -> Result<Option<String>, GitError>;
    /// `/recompile`: every session repins to HEAD on its next turn.
    fn request_refresh(&self);
}

struct LivePin {
    record: ProjectionPinRecord,
    epoch: u64,
}

struct Resolved {
    compaction_id: Option<String>,
    state: Option<ProjectionPinRecord>,
    reason: Option<ProjectionRepinReason>,
}

/// Port of `createProjectionPins`.
pub struct NativeProjectionPins {
    now: Box<dyn Fn() -> f64 + Send + Sync>,
    live: Mutex<BTreeMap<String, LivePin>>,
    refresh_epoch: AtomicU64,
    refresh_requested_at_ms: Mutex<f64>,
}

impl Default for NativeProjectionPins {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeProjectionPins {
    pub fn new() -> Self {
        Self::with_now(Box::new(|| memory_core::support::time::now_millis() as f64))
    }

    /// The upstream `{ now }` option: a fixed clock keeps the pin reproducible in tests.
    pub fn with_now(now: Box<dyn Fn() -> f64 + Send + Sync>) -> Self {
        Self {
            now,
            live: Mutex::new(BTreeMap::new()),
            refresh_epoch: AtomicU64::new(0),
            refresh_requested_at_ms: Mutex::new(f64::NEG_INFINITY),
        }
    }

    fn now_ms(&self) -> f64 {
        (self.now)()
    }

    fn store_live(&self, session_id: &str, record: ProjectionPinRecord) {
        let epoch = self.refresh_epoch.load(Ordering::SeqCst);
        self.live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session_id.to_string(), LivePin { record, epoch });
    }

    fn resolve(
        &self,
        repo: &GitMemoryRepo,
        session_id: &str,
        branch: &[SessionEntry],
    ) -> Result<Resolved, GitError> {
        let compaction_id = latest_compaction_id(branch);
        let cached = self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .map(|live| LivePin {
                record: live.record.clone(),
                epoch: live.epoch,
            });
        let state = match &cached {
            Some(live) => Some(live.record.clone()),
            None => read_pin_record(branch, session_id),
        };
        let stale_by_refresh = match &cached {
            None => state
                .as_ref()
                .is_some_and(|state| state.pinned_at_ms < self.refresh_requested_at()),
            Some(live) => live.epoch < self.refresh_epoch.load(Ordering::SeqCst),
        };
        let reason = if stale_by_refresh {
            Some(ProjectionRepinReason::Refresh)
        } else {
            self.repin_reason(repo, state.as_ref(), cached.is_none(), compaction_id.as_deref())?
        };
        Ok(Resolved {
            compaction_id,
            state,
            reason,
        })
    }

    fn refresh_requested_at(&self) -> f64 {
        *self
            .refresh_requested_at_ms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn repin_reason(
        &self,
        repo: &GitMemoryRepo,
        state: Option<&ProjectionPinRecord>,
        loaded_from_branch: bool,
        compaction_id: Option<&str>,
    ) -> Result<Option<ProjectionRepinReason>, GitError> {
        let Some(state) = state else {
            return Ok(Some(ProjectionRepinReason::FirstTurn));
        };
        if state.compaction_id.as_deref() != compaction_id {
            return Ok(Some(ProjectionRepinReason::Compaction));
        }
        if !loaded_from_branch {
            return Ok(None);
        }
        for revision in [state.revision.as_deref(), state.noticed_through.as_deref()] {
            if let Some(revision) = revision
                && !revision_exists(repo, revision)?
            {
                return Ok(Some(ProjectionRepinReason::MissingRevision));
            }
        }
        Ok(None)
    }
}

impl ProjectionPins for NativeProjectionPins {
    fn request_refresh(&self) {
        self.refresh_epoch.fetch_add(1, Ordering::SeqCst);
        let now = self.now_ms();
        *self
            .refresh_requested_at_ms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = now;
    }

    fn peek(&self, input: ProjectionAdvanceInput<'_>) -> Result<Option<String>, GitError> {
        let resolved = self.resolve(input.repo, input.session_id, input.branch)?;
        let pinned = resolved.reason.is_none().then_some(resolved.state).flatten();
        Ok(match pinned {
            Some(state) => state.revision,
            None => input.head,
        })
    }

    fn advance(
        &self,
        input: ProjectionAdvanceInput<'_>,
        record: &dyn Fn(&ProjectionPinRecord),
    ) -> Result<ProjectionTurn, GitError> {
        let resolved = self.resolve(input.repo, input.session_id, input.branch)?;
        let reason = resolved.reason;
        let pinned = if reason.is_none() {
            resolved.state
        } else {
            None
        };
        let Some(state) = pinned else {
            let fresh = ProjectionPinRecord {
                version: 1,
                session_id: input.session_id.to_string(),
                revision: input.head.clone(),
                noticed_through: input.head.clone(),
                compaction_id: resolved.compaction_id,
                pinned_at_ms: self.now_ms(),
            };
            self.store_live(input.session_id, fresh.clone());
            record(&fresh);
            return Ok(ProjectionTurn {
                revision: input.head,
                changes: None,
                repinned: Some(reason.unwrap_or(ProjectionRepinReason::FirstTurn)),
            });
        };

        self.store_live(input.session_id, state.clone());
        if state.noticed_through == input.head {
            return Ok(ProjectionTurn {
                revision: state.revision,
                changes: None,
                repinned: None,
            });
        }
        let changes = projected_changes_between(
            input.repo,
            state.noticed_through.as_deref(),
            input.head.as_deref(),
            &ProjectedChangesOptions {
                exclude_session_id: Some(input.session_id.to_string()),
            },
        )?;
        let advanced = ProjectionPinRecord {
            noticed_through: input.head.clone(),
            ..state.clone()
        };
        self.store_live(input.session_id, advanced.clone());
        record(&advanced);
        if is_empty_projected_changes(&changes) {
            return Ok(ProjectionTurn {
                revision: state.revision,
                changes: None,
                repinned: None,
            });
        }
        Ok(ProjectionTurn {
            revision: state.revision,
            changes: Some(changes),
            repinned: None,
        })
    }
}

/// The newest record this session wrote; a fork's copied records name the parent session and are
/// skipped.
fn read_pin_record(branch: &[SessionEntry], session_id: &str) -> Option<ProjectionPinRecord> {
    for entry in branch.iter().rev() {
        if entry.kind != "custom"
            || entry.data.get("customType").and_then(Value::as_str)
                != Some(PROJECTION_PIN_ENTRY_TYPE)
        {
            continue;
        }
        let Some(record) = entry.data.get("data").and_then(parse_pin_record) else {
            continue;
        };
        if record.session_id == session_id {
            return Some(record);
        }
    }
    None
}

fn parse_pin_record(value: &Value) -> Option<ProjectionPinRecord> {
    if !value.is_object() || value.get("version").and_then(Value::as_u64) != Some(1) {
        return None;
    }
    let session_id = value.get("sessionId").and_then(Value::as_str)?;
    let revision = nullable_string(value.get("revision"))?;
    let noticed_through = nullable_string(value.get("noticedThrough"))?;
    let compaction_id = nullable_string(value.get("compactionId"))?;
    let pinned_at_ms = value.get("pinnedAtMs").and_then(Value::as_f64)?;
    if !pinned_at_ms.is_finite() {
        return None;
    }
    Some(ProjectionPinRecord {
        version: 1,
        session_id: session_id.to_string(),
        revision,
        noticed_through,
        compaction_id,
        pinned_at_ms,
    })
}

fn nullable_string(value: Option<&Value>) -> Option<Option<String>> {
    match value {
        Some(Value::Null) => Some(None),
        Some(Value::String(text)) => Some(Some(text.clone())),
        _ => None,
    }
}

fn latest_compaction_id(branch: &[SessionEntry]) -> Option<String> {
    for (index, entry) in branch.iter().enumerate().rev() {
        if entry.kind != "compaction" {
            continue;
        }
        return Some(
            entry
                .data
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("index:{index}")),
        );
    }
    None
}

/// Port of `renderProjectedChangesLine`: one notice line, at most 20 listed paths.
pub fn render_projected_changes_line(
    changes: &ProjectedChanges,
    pinned_revision: Option<&str>,
) -> String {
    let groups = [
        ("added", &changes.added),
        ("updated", &changes.updated),
        ("removed", &changes.removed),
    ];
    let mut budget = MAX_LISTED_PATHS;
    let mut parts: Vec<String> = Vec::new();
    for (verb, paths) in groups {
        if paths.is_empty() {
            continue;
        }
        let shown: Vec<String> = paths.iter().take(budget).cloned().collect();
        budget = budget.saturating_sub(shown.len());
        let hidden = paths.len() - shown.len();
        let mut listed = shown;
        if hidden > 0 {
            listed.push(format!("{hidden} more"));
        }
        parts.push(format!("{verb} {}", listed.join(", ")));
    }
    let pin = match pinned_revision {
        Some(revision) => revision.chars().take(7).collect::<String>(),
        None => "an empty memory repo".to_string(),
    };
    format!(
        "- {MEMORY_CHANGES_METADATA_TOKEN} {pin}: {}. The prompt keeps the pinned copy; read a file when it matters now.",
        parts.join("; ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_core::git::{GitCommitAuthor, GitSeedFile, InitializeGitRepoOptions};

    const SESSION: &str = "session-1";

    fn fixture() -> (tempfile::TempDir, GitMemoryRepo) {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = GitMemoryRepo::open(dir.path().join("repo"), "pin-agent").expect("repo");
        repo.init(Some(InitializeGitRepoOptions {
            seed_files: vec![GitSeedFile {
                relative_path: "system/persona.md".to_string(),
                content: "---\ndescription: Persona\n---\nfirst\n".to_string(),
            }],
            ..Default::default()
        }))
        .expect("init");
        (dir, repo)
    }

    fn commit_as(repo: &GitMemoryRepo, session_id: &str, path: &str, body: &str) {
        let full = repo.dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("parent dir");
        }
        std::fs::write(&full, format!("---\ndescription: {path}\n---\n{body}\n")).expect("write");
        repo.commit_write(
            &[path],
            &format!("record {path}\n\nOmo-Writer: memory-tool\nOmo-Session: {session_id}\nOmo-Turn: 1"),
            &GitCommitAuthor {
                agent_id: "pin-agent".to_string(),
                author_name: "Pin Agent".to_string(),
                author_email: None,
            },
        )
        .expect("commit");
    }

    fn pin_entry(record: &ProjectionPinRecord) -> SessionEntry {
        SessionEntry {
            id: "pin-entry".to_string(),
            parent_id: None,
            timestamp: String::new(),
            kind: "custom".to_string(),
            data: serde_json::json!({
                "type": "custom",
                "id": "pin-entry",
                "customType": PROJECTION_PIN_ENTRY_TYPE,
                "data": serde_json::to_value(record).expect("record json"),
            }),
        }
    }

    fn compaction_entry() -> SessionEntry {
        SessionEntry {
            id: "compaction-1".to_string(),
            parent_id: None,
            timestamp: String::new(),
            kind: "compaction".to_string(),
            data: serde_json::json!({
                "type": "compaction",
                "id": "compaction-1",
                "firstKeptEntryId": "m1"
            }),
        }
    }

    fn advance(
        pins: &NativeProjectionPins,
        repo: &GitMemoryRepo,
        branch: &[SessionEntry],
        session_id: &str,
    ) -> (ProjectionTurn, Vec<ProjectionPinRecord>) {
        let records = std::cell::RefCell::new(Vec::new());
        let turn = pins
            .advance(
                ProjectionAdvanceInput {
                    repo,
                    session_id,
                    branch,
                    head: repo.head().expect("head"),
                },
                &|record: &ProjectionPinRecord| records.borrow_mut().push(record.clone()),
            )
            .expect("advance");
        (turn, records.into_inner())
    }

    #[test]
    fn a_first_turn_pins_at_head_and_records_once() {
        let (_dir, repo) = fixture();
        let pins = NativeProjectionPins::with_now(Box::new(|| 42.0));
        let (turn, records) = advance(&pins, &repo, &[], SESSION);

        assert_eq!(turn.revision, repo.head().expect("head"));
        assert_eq!(turn.repinned, Some(ProjectionRepinReason::FirstTurn));
        assert!(turn.changes.is_none());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].session_id, SESSION);
        assert_eq!(records[0].revision, records[0].noticed_through);
        assert_eq!(records[0].compaction_id, None);
        assert_eq!(records[0].pinned_at_ms, 42.0);
    }

    #[test]
    fn another_sessions_change_keeps_the_bytes_and_is_announced_once() {
        let (_dir, repo) = fixture();
        let pins = NativeProjectionPins::with_now(Box::new(|| 1.0));
        let (first, _) = advance(&pins, &repo, &[], SESSION);
        commit_as(&repo, "other-session", "reference/new.md", "new");
        commit_as(&repo, "other-session", "system/persona.md", "second");

        let (second, records) = advance(&pins, &repo, &[], SESSION);
        let (third, _) = advance(&pins, &repo, &[], SESSION);

        assert_eq!(second.revision, first.revision);
        assert_eq!(second.repinned, None);
        let changes = second.changes.expect("announced changes");
        assert_eq!(changes.added, vec!["reference/new.md"]);
        assert_eq!(changes.updated, vec!["system/persona.md"]);
        let line = render_projected_changes_line(&changes, second.revision.as_deref());
        assert!(line.contains(MEMORY_CHANGES_METADATA_TOKEN));
        assert!(line.contains("added reference/new.md"));
        assert!(line.contains("updated system/persona.md"));
        assert!(third.changes.is_none());
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn a_commit_this_session_wrote_is_not_announced() {
        let (_dir, repo) = fixture();
        let pins = NativeProjectionPins::with_now(Box::new(|| 1.0));
        let (first, _) = advance(&pins, &repo, &[], SESSION);
        commit_as(&repo, SESSION, "reference/mine.md", "mine");

        let (second, _) = advance(&pins, &repo, &[], SESSION);

        assert_eq!(second.revision, first.revision);
        assert!(second.changes.is_none());
        assert_eq!(second.repinned, None);
    }

    #[test]
    fn a_body_only_edit_of_an_external_file_is_not_announced() {
        let (_dir, repo) = fixture();
        commit_as(&repo, "other-session", "reference/a.md", "one");
        let pins = NativeProjectionPins::with_now(Box::new(|| 1.0));
        let (first, _) = advance(&pins, &repo, &[], SESSION);
        commit_as(&repo, "other-session", "reference/a.md", "two");

        let (second, _) = advance(&pins, &repo, &[], SESSION);

        assert_eq!(second.revision, first.revision);
        assert!(second.changes.is_none());
    }

    #[test]
    fn a_new_session_compiles_the_new_head() {
        let (_dir, repo) = fixture();
        let pins = NativeProjectionPins::with_now(Box::new(|| 1.0));
        let (first, _) = advance(&pins, &repo, &[], SESSION);
        commit_as(&repo, "other-session", "system/persona.md", "second");

        let (fresh, records) = advance(&pins, &repo, &[], "session-2");

        assert_ne!(fresh.revision, first.revision);
        assert_eq!(fresh.revision, repo.head().expect("head"));
        assert_eq!(fresh.repinned, Some(ProjectionRepinReason::FirstTurn));
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].session_id, "session-2");
    }

    #[test]
    fn a_persisted_pin_reproduces_the_bytes_and_announces_the_change_once() {
        let (_dir, repo) = fixture();
        let original = NativeProjectionPins::with_now(Box::new(|| 1.0));
        let (first, records) = advance(&original, &repo, &[], SESSION);
        commit_as(&repo, "other-session", "reference/new.md", "new");

        let resumed = NativeProjectionPins::with_now(Box::new(|| 2.0));
        let branch = [pin_entry(&records[0])];
        let (second, new_records) = advance(&resumed, &repo, &branch, SESSION);

        assert_eq!(second.revision, first.revision);
        assert_eq!(second.repinned, None);
        assert_eq!(
            second.changes.expect("announced changes").added,
            vec!["reference/new.md"]
        );
        assert_eq!(new_records.len(), 1);
        assert_eq!(new_records[0].noticed_through, repo.head().expect("head"));
    }

    #[test]
    fn a_fork_carrying_the_parent_record_pins_fresh() {
        let (_dir, repo) = fixture();
        let parent = NativeProjectionPins::with_now(Box::new(|| 1.0));
        let (_, records) = advance(&parent, &repo, &[], SESSION);
        commit_as(&repo, "other-session", "system/persona.md", "second");

        let fork = NativeProjectionPins::with_now(Box::new(|| 2.0));
        let branch = [pin_entry(&records[0])];
        let (forked, _) = advance(&fork, &repo, &branch, "session-fork");

        assert_eq!(forked.revision, repo.head().expect("head"));
        assert_eq!(forked.repinned, Some(ProjectionRepinReason::FirstTurn));
    }

    #[test]
    fn a_compaction_repins_to_head() {
        let (_dir, repo) = fixture();
        let pins = NativeProjectionPins::with_now(Box::new(|| 1.0));
        advance(&pins, &repo, &[], SESSION);
        commit_as(&repo, "other-session", "system/persona.md", "second");

        let branch = [compaction_entry()];
        let (repinned, records) = advance(&pins, &repo, &branch, SESSION);

        assert_eq!(repinned.revision, repo.head().expect("head"));
        assert_eq!(repinned.repinned, Some(ProjectionRepinReason::Compaction));
        assert!(repinned.changes.is_none());
        assert_eq!(records[0].compaction_id.as_deref(), Some("compaction-1"));
    }

    #[test]
    fn a_requested_refresh_repins_to_head() {
        let (_dir, repo) = fixture();
        let pins = NativeProjectionPins::with_now(Box::new(|| 1.0));
        advance(&pins, &repo, &[], SESSION);
        commit_as(&repo, "other-session", "system/persona.md", "second");

        pins.request_refresh();
        let (refreshed, _) = advance(&pins, &repo, &[], SESSION);

        assert_eq!(refreshed.revision, repo.head().expect("head"));
        assert_eq!(refreshed.repinned, Some(ProjectionRepinReason::Refresh));
    }

    #[test]
    fn a_pinned_revision_that_no_longer_resolves_repins_to_head() {
        let (_dir, repo) = fixture();
        let vanished = ProjectionPinRecord {
            version: 1,
            session_id: SESSION.to_string(),
            revision: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
            noticed_through: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
            compaction_id: None,
            pinned_at_ms: 0.0,
        };
        let pins = NativeProjectionPins::with_now(Box::new(|| 2.0));
        let branch = [pin_entry(&vanished)];

        let (resumed, records) = advance(&pins, &repo, &branch, SESSION);

        assert_eq!(resumed.revision, repo.head().expect("head"));
        assert_eq!(
            resumed.repinned,
            Some(ProjectionRepinReason::MissingRevision)
        );
        assert_eq!(records[0].revision, repo.head().expect("head"));
    }

    #[test]
    fn peek_reports_head_unpinned_and_the_pin_once_settled() {
        let (_dir, repo) = fixture();
        let pins = NativeProjectionPins::with_now(Box::new(|| 1.0));
        let head = repo.head().expect("head");
        assert_eq!(
            pins.peek(ProjectionAdvanceInput {
                repo: &repo,
                session_id: SESSION,
                branch: &[],
                head: head.clone()
            })
            .expect("peek"),
            head
        );

        let (pinned, _) = advance(&pins, &repo, &[], SESSION);
        assert_eq!(
            pins.peek(ProjectionAdvanceInput {
                repo: &repo,
                session_id: SESSION,
                branch: &[],
                head: repo.head().expect("head")
            })
            .expect("peek"),
            pinned.revision
        );
    }

    #[test]
    fn the_notice_line_caps_the_listed_paths() {
        let changes = ProjectedChanges {
            added: (0..25)
                .map(|index| format!("reference/{index}.md"))
                .collect(),
            updated: Vec::new(),
            removed: Vec::new(),
        };

        let line = render_projected_changes_line(&changes, Some("abcdef0123456789"));

        assert!(
            line.starts_with(&format!(
                "- {MEMORY_CHANGES_METADATA_TOKEN} abcdef0: added reference/0.md"
            ))
        );
        assert!(line.contains("reference/19.md"));
        assert!(!line.contains("reference/20.md"));
        assert!(line.contains("5 more"));
    }
}
