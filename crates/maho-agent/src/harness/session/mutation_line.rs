//! Port of senpi packages/agent/src/harness/session/mutation-line.ts.

use std::sync::Arc;

use tokio::sync::Mutex;

use super::session::SessionError;

/// Serializes complete read-modify-write jobs for one Session.
#[derive(Clone, Default)]
pub struct MutationLine {
    gate: Arc<Mutex<()>>,
    sealed: Arc<Mutex<Option<SessionError>>>,
}

impl MutationLine {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn run<T, F, Fut>(&self, operation: F) -> Result<T, SessionError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, SessionError>>,
    {
        if let Some(error) = self.sealed.lock().await.clone() {
            return Err(error);
        }
        let guard = self.gate.lock().await;
        if let Some(error) = self.sealed.lock().await.clone() {
            drop(guard);
            return Err(error);
        }
        let result = operation().await;
        drop(guard);
        result
    }

    /// Seal queued and future jobs, then drain the running job.
    pub async fn seal(&self, error: SessionError) {
        {
            let mut sealed = self.sealed.lock().await;
            if sealed.is_none() {
                *sealed = Some(error);
            }
        }
        let guard = self.gate.lock().await;
        drop(guard);
    }
}
