//! Port of senpi `packages/coding-agent/src/modes/rpc/loopless-execute-grant.ts`.
//!
//! Who may run tool calls on a loopless runtime (`senpi#5413`). Closed and fail-closed: a runtime
//! built without a grant this module issued refuses every call.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LooplessExecuteCaller {
    InProcessTestHarness,
    RemoteToolExecutor,
}

impl LooplessExecuteCaller {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InProcessTestHarness => "in-process-test-harness",
            Self::RemoteToolExecutor => "remote-tool-executor",
        }
    }
}

/// Sealed grant token: can only be constructed by this module's `grant_loopless_execute_*`
/// constructors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LooplessExecuteGrant {
    caller: LooplessExecuteCaller,
    _sealed: (),
}

impl LooplessExecuteGrant {
    pub fn caller(&self) -> LooplessExecuteCaller {
        self.caller
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LooplessExecuteRefusedError {
    pub reason: String,
}

impl LooplessExecuteRefusedError {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl fmt::Display for LooplessExecuteRefusedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "loopless_execute_refused: {}", self.reason)
    }
}

impl std::error::Error for LooplessExecuteRefusedError {}

/// Issues the test harness's grant; outside a test harness run it refuses.
pub fn grant_loopless_execute_to_test_harness()
-> Result<LooplessExecuteGrant, LooplessExecuteRefusedError> {
    let allowed = cfg!(test)
        || std::env::var("VITEST").as_deref() == Ok("true")
        || std::env::var("MAHO_TEST_HARNESS").as_deref() == Ok("true");
    if !allowed {
        return Err(LooplessExecuteRefusedError::new(
            "only the in-process test harness may execute",
        ));
    }
    Ok(LooplessExecuteGrant {
        caller: LooplessExecuteCaller::InProcessTestHarness,
        _sealed: (),
    })
}

/// Issues the remote tool executor's grant: every call made with it runs the extensions' tool
/// handlers.
pub fn grant_loopless_execute_to_remote_executor() -> LooplessExecuteGrant {
    LooplessExecuteGrant {
        caller: LooplessExecuteCaller::RemoteToolExecutor,
        _sealed: (),
    }
}

/// True when calls made with `grant` must run the extensions' `tool_call`/`tool_result` handlers.
pub fn loopless_grant_runs_tool_handlers(grant: Option<&LooplessExecuteGrant>) -> bool {
    grant.is_some_and(|g| g.caller == LooplessExecuteCaller::RemoteToolExecutor)
}

/// True only for a grant this module issued to an allowed caller.
pub fn is_loopless_execute_granted(grant: Option<&LooplessExecuteGrant>) -> bool {
    grant.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grants_enforce_caller_semantics() {
        let remote = grant_loopless_execute_to_remote_executor();
        assert!(is_loopless_execute_granted(Some(&remote)));
        assert!(loopless_grant_runs_tool_handlers(Some(&remote)));

        let test_grant = grant_loopless_execute_to_test_harness().expect("test grant");
        assert!(is_loopless_execute_granted(Some(&test_grant)));
        assert!(!loopless_grant_runs_tool_handlers(Some(&test_grant)));

        assert!(!is_loopless_execute_granted(None));
        assert!(!loopless_grant_runs_tool_handlers(None));
    }
}
