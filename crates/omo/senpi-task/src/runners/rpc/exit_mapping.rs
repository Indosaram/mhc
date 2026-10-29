//! `runners/rpc/exit-mapping.ts`: classify how a child ended and map it onto status facts.

use crate::runners::types::{ChildExitFacts, ChildExitOutcome, RunnerErrorFacts};

const STDERR_TAIL_CAP: usize = 4_096;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChildExitInput {
    pub code: Option<i32>,
    pub signal: Option<String>,
    /// Spawn error message (`error.message`).
    pub error: Option<String>,
    pub pid: Option<i64>,
    pub stderr: String,
}

/// Keep only the last `cap` characters of a stderr buffer (TS counts UTF-16 units; Rust counts chars).
pub fn tail_stderr(stderr: &str, cap: usize) -> String {
    let count = stderr.chars().count();
    if count <= cap {
        return stderr.to_string();
    }
    stderr.chars().skip(count - cap).collect()
}

/// A spawn error dominates; then exit-by-signal is `killed`; zero is `clean`; anything else `crashed`.
pub fn classify_child_exit(input: &ChildExitInput) -> ChildExitOutcome {
    let facts = ChildExitFacts {
        pid: input.pid,
        code: input.code,
        signal: input.signal.clone(),
        stderr_tail: tail_stderr(&input.stderr, STDERR_TAIL_CAP),
    };
    if let Some(message) = &input.error {
        return ChildExitOutcome::SpawnError {
            message: message.clone(),
            facts,
        };
    }
    if input.signal.is_some() {
        return ChildExitOutcome::Killed { facts };
    }
    if input.code == Some(0) {
        return ChildExitOutcome::Clean { facts };
    }
    ChildExitOutcome::Crashed { facts }
}

/// There is no `killed` status: `killed` is a fact on an `error` status. An exit after a terminal
/// transition is resident teardown and yields `None`.
pub fn map_exit_outcome_to_error(
    outcome: &ChildExitOutcome,
    already_terminal: bool,
) -> Option<RunnerErrorFacts> {
    if already_terminal {
        return None;
    }
    let exit = outcome.facts().clone();
    let (killed, error_message) = match outcome {
        ChildExitOutcome::Killed { .. } => (
            true,
            format!(
                "RPC child killed by signal {} (pid={})",
                exit.signal.as_deref().unwrap_or("null"),
                exit.pid
                    .map_or_else(|| "unknown".to_string(), |pid| pid.to_string())
            ),
        ),
        ChildExitOutcome::Crashed { .. } => {
            let trimmed = exit.stderr_tail.trim();
            let message = if trimmed.is_empty() {
                format!(
                    "RPC child exited with code {}",
                    exit.code
                        .map_or_else(|| "null".to_string(), |code| code.to_string())
                )
            } else {
                trimmed.to_string()
            };
            (false, message)
        }
        ChildExitOutcome::SpawnError { message, .. } => (false, message.clone()),
        ChildExitOutcome::Clean { .. } => (
            false,
            "RPC child exited cleanly before reaching a terminal state".to_string(),
        ),
    };
    Some(RunnerErrorFacts {
        killed,
        error_message,
        exit,
    })
}
