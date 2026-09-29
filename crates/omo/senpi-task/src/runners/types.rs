//! Runner-shared value types (`runners/types.ts`).

use std::collections::BTreeMap;

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RpcSwitchSessionResult {
    pub cancelled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RpcEntriesResult {
    /// Host `SessionEntry` values, untrusted JSON.
    pub entries: Vec<Value>,
    pub leaf_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RpcSpawnSpec {
    pub cwd: String,
    pub extensions: Option<Vec<String>>,
    pub member_env: Option<BTreeMap<String, String>>,
}

/// How a child process ended (`ChildExitFacts`). `signal` is the signal name (`"SIGKILL"`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChildExitFacts {
    pub pid: Option<i64>,
    pub code: Option<i32>,
    pub signal: Option<String>,
    pub stderr_tail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildExitOutcome {
    Clean {
        facts: ChildExitFacts,
    },
    Killed {
        facts: ChildExitFacts,
    },
    Crashed {
        facts: ChildExitFacts,
    },
    SpawnError {
        message: String,
        facts: ChildExitFacts,
    },
}

impl ChildExitOutcome {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Clean { .. } => "clean",
            Self::Killed { .. } => "killed",
            Self::Crashed { .. } => "crashed",
            Self::SpawnError { .. } => "spawn_error",
        }
    }

    pub fn facts(&self) -> &ChildExitFacts {
        match self {
            Self::Clean { facts }
            | Self::Killed { facts }
            | Self::Crashed { facts }
            | Self::SpawnError { facts, .. } => facts,
        }
    }
}

/// Status facts for a child that exited before reaching a terminal state (status is always `error`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerErrorFacts {
    pub killed: bool,
    pub error_message: String,
    pub exit: ChildExitFacts,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerminateOptions {
    pub sigkill_delay_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RpcTerminalAssistantMessage {
    pub text: Option<String>,
    pub stop_reason: Option<String>,
    pub error_message: Option<String>,
}

/// `RpcRunnerSpec`: everything needed to launch (or resume) one RPC child.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RpcRunnerSpec {
    pub task_id: String,
    pub cwd: String,
    pub state_dir: String,
    pub prompt: String,
    pub resume_session_path: Option<String>,
    pub model: Option<String>,
    pub reasoning: Option<String>,
    pub variant: Option<String>,
    pub extensions: Option<Vec<String>>,
    pub member_env: Option<BTreeMap<String, String>>,
}
