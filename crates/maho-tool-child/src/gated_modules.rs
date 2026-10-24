//! The gated capabilities of a tool-child job worker (senpi `tool-child/gated-modules.ts`; senpi#3566, plan todo 4).
//!
//! Upstream replaces `node:child_process` and `node:worker_threads` for extension code with virtual modules. mhc has
//! no module graph to redirect, so the same contract is the list of gated capability names a worker exposes: the
//! launch functions a gated process API replaces, the ids the two redirects used, and the refusal of a nested worker.
//! A module worker is handed `GatedChildProcess` (see `gated_child_process`) in place of the launch functions, and
//! `RefusedWorker` in place of thread creation.

/// The id upstream's virtual `child_process` module is served under.
pub const GATED_CHILD_PROCESS_ID: &str = "senpi-tool-child:child_process";

/// The specifiers that resolve to the gated `child_process` id.
pub const CHILD_PROCESS_REDIRECTS: [(&str, &str); 2] = [
    ("node:child_process", GATED_CHILD_PROCESS_ID),
    ("child_process", GATED_CHILD_PROCESS_ID),
];

/// The launch functions the gated module replaces; every other export is the builtin's own, untouched.
pub const CHILD_PROCESS_LAUNCH_FUNCTIONS: [&str; 7] = [
    "spawn",
    "spawnSync",
    "exec",
    "execSync",
    "execFile",
    "execFileSync",
    "fork",
];

/// The launch functions `GatedChildProcess` implements natively. `fork` has no native analogue (it exists to start a
/// Node child with an IPC channel); a worker that wants a child of this engine uses `spawn` with the re-entry command.
pub const NATIVE_LAUNCH_FUNCTIONS: [&str; 6] = [
    "spawn",
    "spawnSync",
    "exec",
    "execSync",
    "execFile",
    "execFileSync",
];

pub const GATED_WORKER_THREADS_ID: &str = "senpi-tool-child:worker_threads";

/// The specifiers that resolve to the gated `worker_threads` id.
pub const WORKER_THREADS_REDIRECTS: [(&str, &str); 2] = [
    ("node:worker_threads", GATED_WORKER_THREADS_ID),
    ("worker_threads", GATED_WORKER_THREADS_ID),
];

/// The code `NestedWorkerRefusedError` carries.
pub const NESTED_WORKER_REFUSED: &str = "tool_child_nested_worker_refused";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "tool_child_nested_worker_refused: a tool-child job worker cannot start worker threads of its own (their launches would bypass the execution barrier)"
)]
pub struct NestedWorkerRefusedError;

impl NestedWorkerRefusedError {
    pub const NAME: &'static str = "NestedWorkerRefusedError";
    pub const CODE: &'static str = NESTED_WORKER_REFUSED;
}

/// The Worker a job worker sees: starting one always fails, so no thread outside the barrier ever starts.
pub fn refused_worker() -> Result<std::convert::Infallible, NestedWorkerRefusedError> {
    Err(NestedWorkerRefusedError)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_seven_launch_functions_and_the_redirected_specifiers() {
        assert_eq!(CHILD_PROCESS_LAUNCH_FUNCTIONS.len(), 7);
        assert!(
            NATIVE_LAUNCH_FUNCTIONS
                .iter()
                .all(|name| CHILD_PROCESS_LAUNCH_FUNCTIONS.contains(name))
        );
        assert!(
            CHILD_PROCESS_REDIRECTS
                .iter()
                .all(|(_, id)| *id == GATED_CHILD_PROCESS_ID)
        );
        assert!(
            WORKER_THREADS_REDIRECTS
                .iter()
                .all(|(_, id)| *id == GATED_WORKER_THREADS_ID)
        );
    }

    #[test]
    fn a_nested_worker_is_refused_typed() {
        let error = refused_worker().expect_err("refused");
        assert_eq!(
            NestedWorkerRefusedError::CODE,
            "tool_child_nested_worker_refused"
        );
        assert!(
            error
                .to_string()
                .starts_with("tool_child_nested_worker_refused: ")
        );
        assert_eq!(NestedWorkerRefusedError::NAME, "NestedWorkerRefusedError");
    }
}
