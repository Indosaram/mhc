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
    /// Messages delivered by the next run (`task_send` while no live channel exists, and steers
    /// written to a running child, keyed by their gateway delivery id; task.md Delta D7).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_messages: Vec<PendingMessage>,
    /// Providers a first run was moved away from after a credential failure (task.md Delta D4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropped_providers: Vec<String>,
    /// When the current run's child process was started (ms; the pid ownership check of
    /// task.md Delta D8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_started_at_ms: Option<u64>,
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

/// Slack for comparing a process's start time with the recorded spawn time (clock-tick and
/// boot-time rounding).
const START_SLACK_MS: u64 = 2_000;

/// What the operating system reports about a live process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessInfo {
    /// Command line, arguments joined with single spaces.
    pub command_line: String,
    /// Start time in ms since the Unix epoch, when known.
    pub started_at_ms: Option<u64>,
    /// Executable image path or name, when known.
    pub image: Option<String>,
}

/// Information about live process `pid`; `None` when it is gone or cannot be inspected.
#[cfg(target_os = "linux")]
pub fn process_info(pid: u32) -> Option<ProcessInfo> {
    let base = PathBuf::from(format!("/proc/{pid}"));
    let raw = std::fs::read(base.join("cmdline")).ok()?;
    let command_line = raw
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    let started_at_ms = linux_start_ms(&base);
    let image = std::fs::read_link(base.join("exe"))
        .ok()
        .map(|path| path.to_string_lossy().into_owned());
    Some(ProcessInfo {
        command_line,
        started_at_ms,
        image,
    })
}

/// Start time from `/proc/<pid>/stat` field 22 (clock ticks after boot) and `/proc/stat` `btime`.
#[cfg(target_os = "linux")]
fn linux_start_ms(base: &Path) -> Option<u64> {
    let stat = std::fs::read_to_string(base.join("stat")).ok()?;
    // Fields after the parenthesised command name; field 22 is the 20th of those.
    let after = &stat[stat.rfind(')')? + 1..];
    let ticks: u64 = after.split_whitespace().nth(19)?.parse().ok()?;
    let boot = std::fs::read_to_string("/proc/stat").ok()?;
    let btime: u64 = boot
        .lines()
        .find_map(|line| line.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()?;
    // USER_HZ is 100 on every Linux ABI mhc supports.
    Some(btime * 1_000 + ticks * 10)
}

/// Other Unix systems: `ps` (elapsed time and command line).
#[cfg(all(unix, not(target_os = "linux")))]
pub fn process_info(pid: u32) -> Option<ProcessInfo> {
    let output = std::process::Command::new("ps")
        .args(["-ww", "-p", &pid.to_string(), "-o", "etime=", "-o", "command="])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next()?.trim();
    let (elapsed, command) = line.split_once(char::is_whitespace)?;
    let started_at_ms = parse_etime(elapsed).map(|secs| now_ms().saturating_sub(secs * 1_000));
    Some(ProcessInfo {
        command_line: command.trim().to_owned(),
        started_at_ms,
        image: None,
    })
}

/// `[[dd-]hh:]mm:ss` in seconds.
#[cfg(all(unix, not(target_os = "linux")))]
fn parse_etime(text: &str) -> Option<u64> {
    let (days, rest) = match text.split_once('-') {
        Some((days, rest)) => (days.parse::<u64>().ok()?, rest),
        None => (0, text),
    };
    let mut seconds = 0;
    for part in rest.split(':') {
        seconds = seconds * 60 + part.parse::<u64>().ok()?;
    }
    Some(days * 86_400 + seconds)
}

/// Windows: the process's image path and command line through PowerShell/CIM.
#[cfg(windows)]
pub fn process_info(pid: u32) -> Option<ProcessInfo> {
    let script = format!(
        "$p = Get-CimInstance Win32_Process -Filter 'ProcessId={pid}'; if ($p) {{ $p.ExecutablePath; $p.CommandLine }}"
    );
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines();
    let image = lines.next()?.trim().to_owned();
    let command_line = lines.collect::<Vec<_>>().join(" ").trim().to_owned();
    (!image.is_empty()).then_some(ProcessInfo {
        command_line,
        started_at_ms: None,
        image: Some(image),
    })
}

#[cfg(not(any(unix, windows)))]
pub fn process_info(_pid: u32) -> Option<ProcessInfo> {
    None
}

/// Whether `info` describes the child of `record` (task.md Delta D8): its command line carries
/// `--session-id <session id>`; on Unix it did not start after the recorded spawn; on Windows
/// its image is the current executable.
pub fn matches_task_child(record: &TaskRecord, info: &ProcessInfo) -> bool {
    let session = record.session_id.trim();
    let tokens: Vec<&str> = info.command_line.split_whitespace().collect();
    let carries_session = !session.is_empty()
        && tokens.windows(2).any(|pair| pair[0] == "--session-id" && pair[1] == session);
    if !carries_session {
        return false;
    }
    if cfg!(unix)
        && let (Some(started), Some(spawned)) = (info.started_at_ms, record.pid_started_at_ms)
        && started > spawned.saturating_add(START_SLACK_MS)
    {
        return false;
    }
    if cfg!(windows) {
        let own = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.file_name().map(|name| name.to_string_lossy().to_lowercase()));
        let image = info.image.as_deref().map(|image| {
            Path::new(image)
                .file_name()
                .map(|name| name.to_string_lossy().to_lowercase())
                .unwrap_or_default()
        });
        if own.is_none() || image != own {
            return false;
        }
    }
    true
}

/// Whether the recorded pid of `record` is still alive **and** still that task's child.
pub fn is_task_child(record: &TaskRecord) -> bool {
    record
        .pid
        .filter(|pid| pid_alive(*pid) || cfg!(windows))
        .and_then(process_info)
        .is_some_and(|info| matches_task_child(record, &info))
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
        dropped_providers: Vec::new(),
        pid_started_at_ms: None,
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

    /// task.md Delta D8: the command-line and start-time checks.
    #[test]
    fn task_child_match() {
        let mut record = sample("A");
        record.session_id = "sid-1".into();
        record.pid_started_at_ms = Some(10_000);
        let info = |command: &str, started: Option<u64>| ProcessInfo {
            command_line: command.to_owned(),
            started_at_ms: started,
            image: None,
        };
        if cfg!(unix) {
            assert!(matches_task_child(&record, &info("mhc -p --session-id sid-1 --name A", Some(9_000))));
            assert!(!matches_task_child(&record, &info("sleep 30", Some(9_000))));
            assert!(!matches_task_child(&record, &info("mhc --session-id sid-10", Some(9_000))));
            // A process started well after the spawn reuses the pid.
            assert!(!matches_task_child(&record, &info("mhc -p --session-id sid-1", Some(60_000))));
            record.pid_started_at_ms = None;
            assert!(matches_task_child(&record, &info("mhc -p --session-id sid-1", Some(60_000))));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn own_process_info() {
        let info = process_info(std::process::id()).expect("info");
        assert!(!info.command_line.is_empty());
        let started = info.started_at_ms.expect("start");
        assert!(started <= now_ms() + START_SLACK_MS, "{started}");
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
