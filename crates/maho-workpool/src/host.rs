//! What the pool engine needs from the embedding runtime (spec §4, §6, §7), and the worker index
//! that lets a worker process find its pool.
//!
//! [`crate::extension::TaskHost`] implements [`WorkerHost`] over `maho-task`; tests use an
//! in-memory host.
use crate::model::{AgentSpec, PoolError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Called once with the host's answer to an aggregate delivery: `Ok` when the message was
/// written to the session, `Err(reason)` when it was dropped.
pub type DeliveryDone = Box<dyn FnOnce(Result<(), String>) + Send>;

/// A new worker task.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkerStart {
    pub pool_id: String,
    pub pool_name: String,
    pub agent: AgentSpec,
    /// Extra tools granted to the worker (spec §6), sorted.
    pub tools: Vec<String>,
    /// The worker's first prompt: the agent prompt followed by the assignment.
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedWorker {
    pub task_id: String,
    /// The worker's session id (written to the worker index).
    pub session_id: Option<String>,
}

/// Tool names seen at `create` (spec §6).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolCatalog {
    /// Tools of the parent's live runtime.
    pub parent: Vec<String>,
    /// Tools the worker already has.
    pub worker: Vec<String>,
    /// Tools the worker's deny list removes.
    pub denied: Vec<String>,
}

pub trait WorkerHost: Send + Sync {
    /// Spawn policy for `agent` (`policy_denied` on `Err`).
    ///
    /// # Errors
    /// The policy denies the spawn; the message explains why.
    fn check_policy(&self, agent: &AgentSpec) -> Result<(), String>;
    /// Resolves `agent` like a delegated task; the value identifies the resolved worker (agent,
    /// model) and is compared by idempotent `create`.
    ///
    /// # Errors
    /// The agent or model cannot be resolved (`admission_refused`).
    fn resolve(&self, agent: &AgentSpec) -> Result<Value, String>;
    fn tool_catalog(&self, agent: &AgentSpec) -> ToolCatalog;
    /// Whether the parent runtime that provides extra tools is still live.
    fn tools_available(&self, tools: &[String]) -> bool;
    /// Free task slots right now; `None` = unlimited.
    fn free_slots(&self) -> Option<usize>;
    /// Starts a worker task.
    ///
    /// # Errors
    /// `spawn_failed`, `policy_denied` or `admission_refused`.
    fn start(&self, request: &WorkerStart) -> Result<StartedWorker, PoolError>;
    /// Sends `message` as a follow-up run (new run epoch) of an idle worker.
    ///
    /// # Errors
    /// `worker_not_continuable`.
    fn follow_up(&self, task_id: &str, message: &str) -> Result<(), PoolError>;
    /// Resolves when the worker's current run ends.
    fn wait_run(&self, task_id: &str) -> BoxFuture<()>;
    /// `Some(true)` while the worker's run is active, `Some(false)` when it ended, `None` when
    /// the task is unknown.
    fn run_active(&self, task_id: &str) -> Option<bool>;
    fn cancel(&self, task_id: &str);
    /// Queues a `workpool-aggregate` custom message for `session`'s next idle point; the message
    /// triggers a turn.
    fn deliver(&self, session: &str, text: String, details: Value, done: DeliveryDone);
    /// Surfaces a pool event (delivery failure, corrupt record found at session start).
    fn notice(&self, _session: &str, _text: &str) {}
}

/// Where a worker task's pool lives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerEntry {
    /// The parent's task state dir (holds `workpools/`).
    pub state_dir: PathBuf,
    pub pool_id: String,
    pub task_id: String,
    /// Extra tools granted to the worker (spec §6), with the schema the worker registers.
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
}

/// An extra tool as a worker registers it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// `<agent dir>/workpool-workers/<root session>__<task id>.json`. A worker process knows both
/// parts from its environment (`MHC_TASK_ROOT_SESSION`, `MHC_TASK_ID`), so it can find its pool
/// before its session id is known.
#[derive(Debug, Clone)]
pub struct WorkerIndex {
    dir: PathBuf,
}

fn file_safe(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

impl WorkerIndex {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The index of an agent dir.
    pub fn in_agent_dir(agent_dir: &Path) -> Self {
        Self::new(agent_dir.join("workpool-workers"))
    }

    fn path(&self, root_session: &str, task_id: &str) -> PathBuf {
        self.dir.join(format!("{}__{}.json", file_safe(root_session), file_safe(task_id)))
    }

    /// Records that `task_id` (started under `root_session`) is a pool worker.
    ///
    /// # Errors
    /// Filesystem failures.
    pub fn write(&self, root_session: &str, entry: &WorkerEntry) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
        let bytes = serde_json::to_vec(entry).map_err(std::io::Error::other)?;
        let written = std::fs::File::create(&tmp).and_then(|mut file| {
            file.write_all(&bytes)?;
            file.sync_all()
        });
        written
            .and_then(|()| std::fs::rename(&tmp, self.path(root_session, &entry.task_id)))
            .inspect_err(|_| {
                let _ = std::fs::remove_file(&tmp);
            })
    }

    pub fn read(&self, root_session: &str, task_id: &str) -> Option<WorkerEntry> {
        if root_session.trim().is_empty() || task_id.trim().is_empty() {
            return None;
        }
        let bytes = std::fs::read(self.path(root_session, task_id)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}
