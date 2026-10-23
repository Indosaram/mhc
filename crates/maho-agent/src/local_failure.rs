//! Port of senpi packages/agent/src/local-failure.ts.
//!
//! Errors raised by the owner's queued-input port (claim, release, commit) inside a run are local
//! bookkeeping, never a provider request, so the run's failure message is marked
//! `failureSource: "local"` (senpi#3866). Rust tags them with a typed error instead of a WeakSet.

use std::error::Error;
use std::fmt;
use std::sync::Arc;

use maho_ai::types::BoxFuture;

use crate::agent::{QueuedDrain, QueuedGiveBack, QueuedInputPort};
use crate::types::{AgentMessage, PendingQueue};

/// Marker error: the wrapped failure came from local queued-input code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFailure(pub String);

impl fmt::Display for LocalFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for LocalFailure {}

pub fn is_local_failure(error: &(dyn Error + 'static)) -> bool {
    error.is::<LocalFailure>()
}

/// Wraps a port so every error it reports is tagged local. `claim` and `release` cannot fail in
/// the Rust trait; `commit_local` exposes the typed commit error.
pub struct LocalFailurePort {
    inner: Arc<dyn QueuedInputPort>,
}

impl LocalFailurePort {
    pub fn commit_local(
        &self,
        queue: PendingQueue,
        messages: Vec<AgentMessage>,
    ) -> BoxFuture<'static, Result<(), LocalFailure>> {
        let commit = self.inner.commit_consumption(queue, messages);
        Box::pin(async move { commit.await.map_err(LocalFailure) })
    }
}

impl QueuedInputPort for LocalFailurePort {
    fn claim(
        &self,
        queue: PendingQueue,
        drain: QueuedDrain,
    ) -> BoxFuture<'static, Vec<AgentMessage>> {
        self.inner.claim(queue, drain)
    }

    fn release(
        &self,
        queue: PendingQueue,
        messages: Vec<AgentMessage>,
        give_back: QueuedGiveBack,
    ) -> BoxFuture<'static, ()> {
        self.inner.release(queue, messages, give_back)
    }

    fn commit_consumption(
        &self,
        queue: PendingQueue,
        messages: Vec<AgentMessage>,
    ) -> BoxFuture<'static, Result<(), String>> {
        self.inner.commit_consumption(queue, messages)
    }
}

pub fn local_failure_port(port: Arc<dyn QueuedInputPort>) -> Arc<LocalFailurePort> {
    Arc::new(LocalFailurePort { inner: port })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tagged_errors_are_local() {
        assert!(is_local_failure(&LocalFailure("boom".into())));
        assert!(!is_local_failure(&fmt::Error));
    }
}
