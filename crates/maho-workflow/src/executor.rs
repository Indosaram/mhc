//! How a node's child task is started, awaited, steered and cancelled. The scheduler talks only
//! to [`NodeExecutor`]; [`crate::addon::TaskExecutor`] implements it over `maho-task`.
use std::future::Future;
use std::pin::Pin;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What the executor needs to start one node's child.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeRequest {
    pub run_id: String,
    pub node_id: String,
    pub attempt: u32,
    /// The prompt exactly as stored (skills already prepended).
    pub prompt: String,
    pub category: Option<String>,
    pub subagent_type: Option<String>,
    pub model: Option<String>,
    pub label: Option<String>,
}

/// Why a child could not be started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartError {
    /// One of the node error codes (`plan_unresolved`, `depth_denied`, `start_failed`,
    /// `residency_denied`).
    pub code: String,
    pub message: String,
}

/// How a child ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChildOutcome {
    Completed { output: String },
    /// `infrastructure` marks problems outside the child's own work (child unresponsive, host
    /// unavailable, session could not be resumed); only those are retried automatically.
    Failed { message: String, infrastructure: bool },
    Cancelled,
    /// The process that ran the child ended before it reported back.
    Interrupted { message: String },
}

/// What recovery finds for a child id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChildProbe {
    Running,
    Settled(ChildOutcome),
    /// The task exists but its child is gone without a result.
    Lost,
    /// No task with that id exists.
    Missing,
}

/// Result of `send`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    /// The message reached the child's current run.
    Steered,
    /// The child was idle; the message started a follow-up run in its session.
    FollowUp,
    NotContinuable,
}

pub trait NodeExecutor: Send + Sync {
    /// Starts a child; returns its task id.
    ///
    /// # Errors
    /// The child could not be started.
    fn start(&self, request: &NodeRequest) -> Result<String, StartError>;
    /// Resolves when the child settles.
    fn wait(&self, task_id: &str) -> BoxFuture<'static, ChildOutcome>;
    fn cancel(&self, task_id: &str);
    fn send(&self, task_id: &str, message: &str) -> SendOutcome;
    /// State of a child found in a recovered run.
    fn probe(&self, task_id: &str) -> ChildProbe;
    /// Time (ms) of the child's last observed activity, when known.
    fn last_activity(&self, _task_id: &str) -> Option<u64> {
        None
    }
}
