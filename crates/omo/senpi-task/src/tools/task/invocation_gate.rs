//! `tools/task/invocation-gate.ts`: tool-layer bridge between the harness-neutral invocation guard
//! and the per-call session.

use crate::agents::{
    EmptySkillInvocations, InvocationGuardVerdict, SkillInvocationState,
    evaluate_invocation_guard, invocation_condition_for_agent,
};

/// `TaskToolDeps.resolveSkillInvocations`: resolves a session's skill-invocation state.
pub type SkillInvocationResolver = dyn Fn(&str) -> Box<dyn SkillInvocationState> + Send + Sync;

/// Runs `f` against the session's skill-invocation state; a missing resolver fails CLOSED into
/// the empty state.
pub fn with_session_skill_state<R>(
    resolver: Option<&SkillInvocationResolver>,
    session_id: &str,
    f: impl FnOnce(&dyn SkillInvocationState) -> R,
) -> R {
    match resolver {
        Some(resolve) => {
            let state = resolve(session_id);
            f(state.as_ref())
        }
        None => f(&EmptySkillInvocations),
    }
}

/// Returns the denial message, or `None` when the spawn may proceed. A missing resolver fails
/// CLOSED - without session state there is no proof the required skill was invoked.
pub fn invocation_gate_denial(
    resolver: Option<&SkillInvocationResolver>,
    subagent_type: &str,
    session_id: &str,
) -> Option<String> {
    invocation_condition_for_agent(subagent_type)?;
    with_session_skill_state(resolver, session_id, |state| {
        if let InvocationGuardVerdict::Deny { message } =
            evaluate_invocation_guard(subagent_type, state)
        {
            Some(message.to_string())
        } else {
            None
        }
    })
}
