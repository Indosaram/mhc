//! The delegated-task board (spec 5 Agents), fed from the `maho-task` state directory.

use serde_json::Value;
use std::path::{Path, PathBuf};

/// Tasks kept on the board.
pub const CAPACITY: usize = 12;

/// Panel status of a task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Queued,
    Running,
    Finished,
    Parked,
}

impl Status {
    /// Maps a task record status (task.md §2).
    pub fn from_record(status: &str) -> Option<Self> {
        Some(match status {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "completed" => Self::Finished,
            "failed" | "cancelled" | "interrupted" => Self::Parked,
            _ => return None,
        })
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Self::Queued => "○",
            Self::Running => "●",
            Self::Finished => "✓",
            Self::Parked => "‖",
        }
    }
}

/// One task as the panel knows it. Partial updates keep known fields.
#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub id: String,
    pub name: String,
    pub status: Status,
    /// Raw record status (`interrupted`, `failed`, ...).
    pub raw_status: String,
    pub reason: Option<String>,
    pub activity: Option<String>,
    pub started_at_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    pub cost: f64,
    /// Order the task was first seen (age for eviction).
    pub seen: u64,
    /// Order the task reached a terminal state.
    pub finished_seq: Option<u64>,
    pub record: Value,
}

/// A partial task update (one record read).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TaskUpdate {
    pub id: String,
    pub description: Option<String>,
    pub name: Option<String>,
    pub agent: Option<String>,
    pub status: Option<String>,
    pub error: Option<String>,
    pub activity: Option<String>,
    pub started_at_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    pub cost: Option<f64>,
    pub owner_session: Option<String>,
    pub record: Value,
}

fn non_empty(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

impl TaskUpdate {
    /// Reads the fields the panel uses from a task record JSON value.
    pub fn from_record(record: &Value) -> Option<Self> {
        let id = non_empty(record.get("id"))?;
        Some(Self {
            id,
            description: non_empty(record.get("description")),
            name: non_empty(record.get("name")),
            agent: non_empty(record.get("agent")),
            status: non_empty(record.get("status")),
            error: non_empty(record.get("error")),
            activity: non_empty(record.get("activity")),
            started_at_ms: record.get("started_at_ms").and_then(Value::as_u64),
            finished_at_ms: record.get("finished_at_ms").and_then(Value::as_u64),
            cost: record
                .get("cost")
                .or_else(|| record.get("cost_usd"))
                .and_then(Value::as_f64),
            owner_session: non_empty(record.get("owner_session")),
            record: record.clone(),
        })
    }

    fn display_name(&self) -> Option<String> {
        self.description
            .clone()
            .or_else(|| self.name.clone())
            .or_else(|| self.agent.clone())
    }
}

/// Counts for the heading.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub running: usize,
    pub parked: usize,
    pub done: usize,
}

/// The board: at most [`CAPACITY`] tasks; active tasks are never dropped.
#[derive(Clone, Debug, Default)]
pub struct TaskBoard {
    tasks: Vec<Task>,
    seq: u64,
    /// Spend of tasks that reached a terminal state (each counted once).
    pub spend: f64,
}

impl TaskBoard {
    /// Applies one update, merging with known fields.
    pub fn apply(&mut self, update: TaskUpdate) {
        self.seq += 1;
        let seq = self.seq;
        let index = match self.tasks.iter().position(|t| t.id == update.id) {
            Some(index) => index,
            None => {
                let Some(status) = update.status.as_deref().and_then(Status::from_record) else {
                    return;
                };
                self.tasks.push(Task {
                    id: update.id.clone(),
                    name: update.id.clone(),
                    status,
                    raw_status: String::new(),
                    reason: None,
                    activity: None,
                    started_at_ms: None,
                    finished_at_ms: None,
                    cost: 0.0,
                    seen: seq,
                    finished_seq: None,
                    record: Value::Null,
                });
                self.tasks.len() - 1
            }
        };
        let task = &mut self.tasks[index];
        let was_terminal = task.finished_seq.is_some();
        if let Some(name) = update.display_name() {
            task.name = name;
        }
        if let Some(raw) = update.status.as_deref()
            && let Some(status) = Status::from_record(raw)
        {
            task.status = status;
            task.raw_status = raw.to_owned();
        }
        if update.error.is_some() {
            task.reason = update.error;
        }
        if update.activity.is_some() {
            task.activity = update.activity;
        }
        if update.started_at_ms.is_some() {
            task.started_at_ms = update.started_at_ms;
        }
        if update.finished_at_ms.is_some() {
            task.finished_at_ms = update.finished_at_ms;
        }
        if let Some(cost) = update.cost {
            task.cost = cost;
        }
        if !update.record.is_null() {
            task.record = update.record;
        }
        if task.status.is_active() {
            task.finished_seq = None;
        } else if !was_terminal {
            task.finished_seq = Some(seq);
            self.spend += task.cost;
        }
        self.evict();
    }

    fn evict(&mut self) {
        while self.tasks.len() > CAPACITY {
            let oldest = self
                .tasks
                .iter()
                .enumerate()
                .filter(|(_, t)| !t.status.is_active())
                .min_by_key(|(_, t)| t.seen)
                .map(|(i, _)| i);
            match oldest {
                Some(i) => {
                    self.tasks.remove(i);
                }
                None => break,
            }
        }
    }

    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    pub fn get(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    pub fn any_active(&self) -> bool {
        self.tasks.iter().any(|t| t.status.is_active())
    }

    pub fn counts(&self) -> Counts {
        let mut counts = Counts::default();
        for task in &self.tasks {
            match task.status {
                Status::Queued | Status::Running => counts.running += 1,
                Status::Parked => counts.parked += 1,
                Status::Finished => counts.done += 1,
            }
        }
        counts
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// The task state directory of `session_id`: `<agent dir>/tasks/<id>` with characters outside
/// `[A-Za-z0-9_-]` replaced by `_` (task.md §2), unless `task.state_dir` overrides it.
pub fn state_dir(
    agent_dir: &Path,
    cwd: &Path,
    override_dir: Option<&str>,
    session_id: &str,
) -> PathBuf {
    if let Some(dir) = override_dir {
        return cwd.join(dir);
    }
    let safe: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    agent_dir.join("tasks").join(safe)
}

/// Reads all task records under `dir` that belong to `session_id`. `None` session → no tasks.
/// An unreadable directory is an error (the caller logs it and keeps the list unchanged).
pub fn read_tasks(dir: &Path, session_id: Option<&str>) -> std::io::Result<Vec<TaskUpdate>> {
    let Some(session_id) = session_id.filter(|s| !s.is_empty()) else {
        return Ok(Vec::new());
    };
    let mut updates = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(update) = TaskUpdate::from_record(&record) else {
            continue;
        };
        if update
            .owner_session
            .as_deref()
            .is_some_and(|owner| owner != session_id)
        {
            continue;
        }
        updates.push(update);
    }
    updates.sort_by_key(|u| u.started_at_ms.unwrap_or(0));
    Ok(updates)
}
