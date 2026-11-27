//! Port of `packages/coding-agent/src/core/kill-guard/types.ts` (senpi#5349).

use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// R1 BROAD_PATTERN, R2 PROTECTED_TARGET, R3 ALL_PROCESSES, R4 GROUP, R5 UNKNOWN (senpi#5349).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KillGuardRule {
    R1,
    R2,
    R3,
    R4,
    R5,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessRow {
    pub pid: i64,
    pub ppid: i64,
    pub pgid: i64,
    /// 0 where the platform does not report it (macOS `ps`).
    pub sid: i64,
    pub uid: u32,
    pub comm: String,
    pub args: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelectResult {
    Ok { pids: Vec<i64> },
    Err { reason: String },
}

pub type SelectFn = Arc<dyn Fn(&[String]) -> SelectResult + Send + Sync>;

/// One read of the process table, plus the host's own `pgrep` run with the given arguments and no signal.
#[derive(Clone)]
pub struct ProcessView {
    pub rows: Vec<ProcessRow>,
    pub select: SelectFn,
}

impl ProcessView {
    pub fn select(&self, args: &[String]) -> SelectResult {
        (self.select)(args)
    }
}

/// The process that will issue the signal: a fresh shell in its own session and group (the PTY
/// bash tool, the core bash tool, monitors), the agent itself (an in-thread JS cell), or another
/// process (a py kernel, a JS process kernel).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillGuardVantage {
    FreshSession,
    Agent,
    Process { pid: i64 },
}

/// What reads the command text: `bash` (the bash tool; a later runtime layer wraps its builtin
/// `kill`), `sh` (a POSIX shell no wrapper reaches: `child_process.exec`, py `os.system`), `bun`
/// (`Bun.$`, which has no `kill` builtin), `argv` (a spawn argv, no shell).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KillGuardShell {
    Bash,
    Sh,
    Bun,
    Argv,
}

impl KillGuardShell {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Sh => "sh",
            Self::Bun => "bun",
            Self::Argv => "argv",
        }
    }
}

pub type ViewFn = Arc<dyn Fn() -> Option<ProcessView> + Send + Sync>;

#[derive(Clone)]
pub struct KillGuardContext {
    pub platform: String,
    pub agent_pid: i64,
    /// The agent's parent, protected even when the table cannot be read.
    pub agent_ppid: Option<i64>,
    pub vantage: KillGuardVantage,
    pub shell: KillGuardShell,
    /// Read once per decision and only when a rule needs it; `None` when the table cannot be read.
    pub view: ViewFn,
}

impl KillGuardContext {
    pub fn view(&self) -> Option<ProcessView> {
        (self.view)()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum KillGuardVerdict {
    Allow,
    /// A dynamic argument the runtime layer will judge; it runs unjudged until that layer exists.
    Defer {
        reason: String,
    },
    Refuse {
        rule: KillGuardRule,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SignalId {
    Number(i64),
    Name(String),
}

/// `process.kill(target, signal)` / `os.kill` (`group: false`) or `os.killpg(target, signal)` (`group: true`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignalCall {
    pub target: i64,
    pub group: bool,
    pub signal: Option<SignalId>,
}
