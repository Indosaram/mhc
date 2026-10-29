//! Persisted task state: one `<id>.json` record per task in the state directory, next to the
//! child's final answer (`<id>.md`), its stderr log (`<id>.log`) and its session files
//! (`sessions/`).
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Waiting for a concurrency slot.
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    /// The process that owned the run ended before the child reported back.
    Interrupted,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }
    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}

/// A message waiting for the task's next run (`docs/cleanroom/specs/task-control.md` §1.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingMessage {
    pub id: String,
    pub text: String,
    /// Milliseconds since the Unix epoch.
    pub created_at: u64,
}

/// One delegated task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: String,
    pub agent: String,
    pub description: String,
    pub status: TaskStatus,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub model_source: String,
    /// Session id of the child, reused by every run of this task (resume).
    pub session_id: String,
    pub cwd: PathBuf,
    pub background: bool,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    /// Number of runs (1 + resumes).
    pub runs: u32,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    /// Files the last run changed (when change tracking was on).
    #[serde(default)]
    pub changed_files: Vec<String>,
    /// Text the post-run hooks appended to the last result.
    #[serde(default)]
    pub hook_notes: Vec<String>,
    /// Tool allowlist of the child (kept for resumes).
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    /// Tools removed from the child (kept for resumes).
    #[serde(default)]
    pub exclude_tools: Vec<String>,
    #[serde(default)]
    pub isolated: bool,
    /// Team (or other grouping) the task belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Show the run in a terminal pane (`None` = the pane observer's default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<bool>,
    /// No generic background-result delivery (the group reports instead).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub quiet: bool,
    /// The name the task was spawned with (its id may carry a `-2`, `-3` suffix).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Session that spawned the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_session: Option<String>,
    /// Root session of the owner (the owner itself for a top-level session).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_session: Option<String>,
    /// Messages delivered by the next run (`task_send` while no live channel exists).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_messages: Vec<PendingMessage>,
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// File layout of one state directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateDir {
    pub root: PathBuf,
}

impl StateDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn record_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.json"))
    }
    pub fn output_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.md"))
    }
    pub fn log_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.log"))
    }
    pub fn prompt_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.prompt.md"))
    }
    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    /// Writes `record` atomically (temp file + rename).
    ///
    /// # Errors
    /// Filesystem errors.
    pub fn save(&self, record: &TaskRecord) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let path = self.record_path(&record.id);
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_vec_pretty(record)?)?;
        std::fs::rename(temp, path)
    }

    pub fn load(&self, id: &str) -> Option<TaskRecord> {
        let bytes = std::fs::read(self.record_path(id)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Every record, oldest first.
    pub fn load_all(&self) -> Vec<TaskRecord> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut records: Vec<TaskRecord> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let id = name.strip_suffix(".json")?;
                self.load(id)
            })
            .collect();
        records.sort_by(|a, b| a.started_at_ms.cmp(&b.started_at_ms).then(a.id.cmp(&b.id)));
        records
    }

    /// The last run's final answer.
    pub fn read_output(&self, id: &str) -> String {
        std::fs::read_to_string(self.output_path(id)).unwrap_or_default()
    }

    /// The tail of the last run's stderr log (at most `max` bytes, on a char boundary).
    pub fn read_log_tail(&self, id: &str, max: usize) -> String {
        let text = std::fs::read_to_string(self.log_path(id)).unwrap_or_default();
        if text.len() <= max {
            return text;
        }
        let mut start = text.len() - max;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        text[start..].to_owned()
    }
}

/// Whether process `pid` is alive.
#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw), None).is_ok()
}

#[cfg(not(unix))]
pub fn pid_alive(_pid: u32) -> bool {
    false
}

/// Marks a record `interrupted` when it is active but no live process owns it (its owner process
/// ended). Returns whether the record changed.
pub fn reconcile(record: &mut TaskRecord, owned_here: bool) -> bool {
    if !record.status.is_active() || owned_here {
        return false;
    }
    if record.status == TaskStatus::Running && record.pid.is_some_and(pid_alive) {
        return false;
    }
    record.status = TaskStatus::Interrupted;
    record.finished_at_ms.get_or_insert_with(now_ms);
    true
}

/// The default state directory of a parent session: `<agent dir>/tasks/<session id>`.
pub fn default_state_root(agent_dir: &Path, session_id: &str) -> PathBuf {
    let session: String = session_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let session = if session.is_empty() { "default".to_owned() } else { session };
    agent_dir.join("tasks").join(session)
}

#[cfg(test)]
pub(crate) fn sample(id: &str) -> TaskRecord {
    TaskRecord {
        id: id.to_owned(),
        agent: "task".into(),
        description: "do it".into(),
        status: TaskStatus::Running,
        model: None,
        thinking: None,
        model_source: "parent".into(),
        session_id: "s".into(),
        cwd: PathBuf::from("/"),
        background: false,
        pid: None,
        exit_code: None,
        error: None,
        runs: 1,
        started_at_ms: 1,
        finished_at_ms: None,
        changed_files: Vec::new(),
        hook_notes: Vec::new(),
        tools: None,
        exclude_tools: Vec::new(),
        isolated: false,
        group: None,
        pane: None,
        quiet: false,
        name: None,
        owner_session: None,
        root_session: None,
        pending_messages: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_and_reconcile() {
        let dir = tempfile::tempdir().expect("tmp");
        let state = StateDir::new(dir.path().join("s"));
        let mut record = sample("A");
        state.save(&record).expect("save");
        assert_eq!(state.load("A"), Some(record.clone()));
        assert_eq!(state.load_all().len(), 1);
        assert!(!reconcile(&mut record, true));
        assert!(reconcile(&mut record, false));
        assert_eq!(record.status, TaskStatus::Interrupted);
        assert!(!reconcile(&mut record, false));
    }

    #[test]
    fn log_tail_respects_char_boundaries() {
        let dir = tempfile::tempdir().expect("tmp");
        let state = StateDir::new(dir.path());
        std::fs::write(state.log_path("A"), "ab\u{e9}cd").expect("write");
        assert_eq!(state.read_log_tail("A", 3), "cd");
        assert_eq!(state.read_log_tail("A", 100), "ab\u{e9}cd");
    }

    #[test]
    fn default_root_sanitizes_session() {
        assert_eq!(
            default_state_root(Path::new("/a"), "x/y"),
            PathBuf::from("/a/tasks/x_y")
        );
    }
}
