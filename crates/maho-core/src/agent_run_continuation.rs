//! Port of senpi `packages/coding-agent/src/core/agent-run-continuation.ts`.
//!
//! Whether the agent core continued a run after its `agent_end` (senpi#3650 review H1, senpi#3351).
//! The core drains input queued during `agent_end` into a new loop inside the same run, while the
//! session settles the run from its own event queue. Observed in emission order, a later `agent_start`
//! proves the core continued, so the settle steps aside and the continuation's own `agent_end` settles
//! the run, whatever the timing.
//!
//! TS keys `agent_end` events by object identity (a WeakMap). Rust events are values, so `observe`
//! returns a token for each `agent_end`, and the settle passes that token back.

use std::future::Future;
use std::sync::Mutex;

use maho_agent::AgentEvent;
use tokio::sync::watch;

/// Identifies one observed `agent_end`: the number of `agent_start`s seen before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentEndToken(u64);

pub struct AgentRunContinuations {
    starts: Mutex<u64>,
    started: watch::Sender<u64>,
}

impl Default for AgentRunContinuations {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentRunContinuations {
    pub fn new() -> Self {
        Self {
            starts: Mutex::new(0),
            started: watch::channel(0).0,
        }
    }

    fn starts(&self) -> u64 {
        self.starts.lock().map(|starts| *starts).unwrap_or(0)
    }

    /// Call synchronously for every agent event, in emission order. Returns the token of an `agent_end`.
    pub fn observe(&self, event: &AgentEvent) -> Option<AgentEndToken> {
        match event {
            AgentEvent::AgentEnd { .. } => Some(AgentEndToken(self.starts())),
            AgentEvent::AgentStart => {
                let next = {
                    let Ok(mut starts) = self.starts.lock() else {
                        return None;
                    };
                    *starts += 1;
                    *starts
                };
                self.started.send_replace(next);
                None
            }
            _ => None,
        }
    }

    /// Resolves true once the core starts another loop after `agent_end`, or false once the agent is
    /// idle. Without an observed `agent_end` it only waits for idle.
    pub async fn idle_or_continued<F>(
        &self,
        agent_end: Option<AgentEndToken>,
        wait_for_idle: F,
    ) -> bool
    where
        F: Future<Output = ()>,
    {
        let Some(AgentEndToken(base)) = agent_end else {
            wait_for_idle.await;
            return false;
        };
        let mut started = self.started.subscribe();
        if self.starts() > base {
            return true;
        }
        let continued = started.wait_for(|starts| *starts > base);
        tokio::select! {
            result = continued => result.is_ok(),
            () = wait_for_idle => self.starts() > base,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn end() -> AgentEvent {
        AgentEvent::AgentEnd {
            messages: Vec::new(),
        }
    }

    #[tokio::test]
    async fn a_start_after_the_end_reports_a_continuation() {
        let runs = AgentRunContinuations::new();
        runs.observe(&AgentEvent::AgentStart);
        let token = runs.observe(&end());
        runs.observe(&AgentEvent::AgentStart);
        assert!(runs.idle_or_continued(token, std::future::pending()).await);
    }

    #[tokio::test]
    async fn idle_without_a_new_start_reports_none() {
        let runs = AgentRunContinuations::new();
        runs.observe(&AgentEvent::AgentStart);
        let token = runs.observe(&end());
        assert!(!runs.idle_or_continued(token, async {}).await);
        assert!(!runs.idle_or_continued(None, async {}).await);
    }

    #[tokio::test]
    async fn a_start_while_waiting_wakes_the_settle() {
        let runs = std::sync::Arc::new(AgentRunContinuations::new());
        let token = runs.observe(&end());
        let waiter = runs.clone();
        let handle = tokio::spawn(async move {
            waiter
                .idle_or_continued(token, std::future::pending())
                .await
        });
        tokio::task::yield_now().await;
        runs.observe(&AgentEvent::AgentStart);
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
        assert!(matches!(result, Ok(Ok(true))));
    }
}
