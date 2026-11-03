//! Identity of one validated tool-call input, carried from preflight to execution.
//!
//! senpi's AgentSession passes the validated `input` object unchanged from the `tool_call` hook to
//! the tool body, and permission dispatch keys approvals on that object (a `WeakMap`). Identical
//! calls, including duplicate provider IDs, get distinct objects. Rust passes owned JSON values, so
//! the agent loop issues one [`InvocationInput`] per prepared call instead and scopes the preflight
//! hook, the execution and the result hook to it.
use std::{
    future::Future,
    sync::{Arc, Weak},
};
use tokio::task::futures::TaskLocalFuture;

tokio::task_local! {
    static CURRENT: InvocationInput;
}

/// One prepared call's input identity; clones compare equal, distinct calls never do.
#[derive(Clone, Debug, Default)]
pub struct InvocationInput(Arc<()>);

impl InvocationInput {
    pub fn new() -> Self {
        Self::default()
    }
    /// A key that stays unique while any clone of this identity is alive.
    pub fn key(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }
    /// Observes the identity without keeping it alive (the `WeakMap` key role).
    pub fn downgrade(&self) -> WeakInvocationInput {
        WeakInvocationInput(Arc::downgrade(&self.0))
    }
}
impl PartialEq for InvocationInput {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for InvocationInput {}

/// A weak observer of an [`InvocationInput`]; dead once every call holder dropped it.
#[derive(Clone, Debug)]
pub struct WeakInvocationInput(Weak<()>);
impl WeakInvocationInput {
    pub fn is_alive(&self) -> bool {
        self.0.strong_count() > 0
    }
}

/// The input identity of the call whose hook or body is running, if any.
pub fn current_invocation_input() -> Option<InvocationInput> {
    CURRENT.try_with(Clone::clone).ok()
}

/// Run `future` as part of the call identified by `input`.
pub fn scope_invocation_input<F: Future>(
    input: InvocationInput,
    future: F,
) -> impl Future<Output = F::Output> {
    CURRENT.scope(input, future)
}

/// Call `start` and poll the future it returns as part of the call identified by `input`, so a
/// hook or tool body sees the identity in its synchronous prologue as well as after it awaits.
///
/// Returns the concrete future (not `impl Future`) so it does not capture `start`'s borrows and
/// stays `'static` whenever `F` is.
pub fn invoke_with_input<F: Future>(
    input: InvocationInput,
    start: impl FnOnce() -> F,
) -> TaskLocalFuture<InvocationInput, F> {
    let future = CURRENT.sync_scope(input.clone(), start);
    CURRENT.scope(input, future)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scoped_identity_is_visible_and_distinct_per_call() {
        assert!(current_invocation_input().is_none());
        let first = InvocationInput::new();
        let second = InvocationInput::new();
        assert_ne!(first, second);
        let seen = scope_invocation_input(first.clone(), async { current_invocation_input() })
            .await
            .expect("scoped identity");
        assert_eq!(seen, first);
        assert_eq!(seen.key(), first.key());
        let weak = second.downgrade();
        drop(second);
        assert!(!weak.is_alive());
    }
}
