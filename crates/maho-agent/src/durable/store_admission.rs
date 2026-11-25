//! Port of senpi `packages/agent/src/durable/store-admission.ts`.

use std::future::Future;
use std::sync::Arc;

use tokio::sync::Mutex;

/// The single admission line above a DurableStore: mutations run one at a time, in admission order.
#[derive(Clone, Default)]
pub struct AdmissionLine {
    gate: Arc<Mutex<()>>,
}

impl AdmissionLine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `mutation` after every earlier admission settles. A rejected mutation releases the line.
    pub async fn admit<T, F, Fut>(&self, mutation: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let _guard = self.gate.lock().await;
        mutation().await
    }
}

pub fn create_admission_line() -> AdmissionLine {
    AdmissionLine::new()
}
