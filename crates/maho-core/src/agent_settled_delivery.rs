//! Port of senpi packages/coding-agent/src/core/agent-settled-delivery.ts.

use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferredTurnDisposition {
    Started,
    Delegated,
    FinishedWithoutStart,
}

/// A deferred turn that resolves once its disposition is known. Resolving twice is a no-op, so a
/// cancelled claim keeps its first disposition.
#[derive(Debug)]
pub struct DeferredTurnClaim {
    resolve: Arc<Mutex<Option<oneshot::Sender<DeferredTurnDisposition>>>>,
    receiver: Arc<tokio::sync::Mutex<Option<oneshot::Receiver<DeferredTurnDisposition>>>>,
}

impl DeferredTurnClaim {
    pub fn new() -> Self {
        let (sender, receiver) = oneshot::channel();
        Self {
            resolve: Arc::new(Mutex::new(Some(sender))),
            receiver: Arc::new(tokio::sync::Mutex::new(Some(receiver))),
        }
    }

    pub fn resolve(&self, disposition: DeferredTurnDisposition) {
        if let Some(sender) = self.resolve.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
            let _ = sender.send(disposition);
        }
    }

    /// Awaits the disposition; returns None when the sender was dropped without resolving.
    pub async fn disposition(&self) -> Option<DeferredTurnDisposition> {
        let receiver = self.receiver.lock().await.take();
        match receiver {
            Some(receiver) => receiver.await.ok(),
            None => None,
        }
    }
}

impl Default for DeferredTurnClaim {
    fn default() -> Self {
        Self::new()
    }
}

pub type DeferredAgentSettledAction = Box<dyn FnOnce() + Send>;

#[derive(Default)]
pub struct DeferredAgentSettledBatch {
    pub actions: Vec<DeferredAgentSettledAction>,
    pub turn_claims: Vec<Arc<DeferredTurnClaim>>,
}

/// Collects actions deferred until the agent settles for one abort generation; a generation
/// mismatch at finish drops the batch instead of running it.
#[derive(Default)]
pub struct AgentSettledDelivery {
    generation: Option<u64>,
    actions: Vec<DeferredAgentSettledAction>,
    turn_claims: Vec<Arc<DeferredTurnClaim>>,
}

impl AgentSettledDelivery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn begin(&mut self, user_abort_generation: u64) {
        self.generation = Some(user_abort_generation);
        self.actions.clear();
        self.turn_claims.clear();
    }

    pub fn defer(&mut self, action: DeferredAgentSettledAction) -> bool {
        if self.generation.is_none() {
            return false;
        }
        self.actions.push(action);
        true
    }

    pub fn defer_trigger_turn(&mut self, action: impl FnOnce(Arc<DeferredTurnClaim>) + Send + 'static) -> bool {
        if self.generation.is_none() {
            return false;
        }
        let claim = Arc::new(DeferredTurnClaim::new());
        self.turn_claims.push(Arc::clone(&claim));
        self.actions.push(Box::new(move || action(claim)));
        true
    }

    pub fn finish(&mut self, user_abort_generation: u64) -> DeferredAgentSettledBatch {
        let batch = if self.generation == Some(user_abort_generation) {
            DeferredAgentSettledBatch {
                actions: std::mem::take(&mut self.actions),
                turn_claims: std::mem::take(&mut self.turn_claims),
            }
        } else {
            DeferredAgentSettledBatch::default()
        };
        self.generation = None;
        self.actions.clear();
        self.turn_claims.clear();
        batch
    }

    pub fn cancel(&mut self) {
        for claim in &self.turn_claims {
            claim.resolve(DeferredTurnDisposition::FinishedWithoutStart);
        }
        self.generation = None;
        self.actions.clear();
        self.turn_claims.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn deferring_before_begin_is_refused() {
        let mut delivery = AgentSettledDelivery::new();
        assert!(!delivery.defer(Box::new(|| {})));
    }

    #[test]
    fn a_matching_generation_returns_the_batch() {
        let mut delivery = AgentSettledDelivery::new();
        let ran = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&ran);
        delivery.begin(1);
        assert!(delivery.defer(Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        })));
        let mut batch = delivery.finish(1);
        assert_eq!(batch.actions.len(), 1);
        for action in batch.actions.drain(..) {
            action();
        }
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_generation_mismatch_drops_the_batch() {
        let mut delivery = AgentSettledDelivery::new();
        delivery.begin(1);
        delivery.defer(Box::new(|| {}));
        let batch = delivery.finish(2);
        assert!(batch.actions.is_empty());
        assert!(batch.turn_claims.is_empty());
    }

    #[test]
    fn cancelling_resolves_pending_turn_claims() {
        let mut delivery = AgentSettledDelivery::new();
        delivery.begin(1);
        delivery.defer_trigger_turn(|_claim| {});
        delivery.cancel();
        assert!(delivery.generation.is_none());
    }

    #[tokio::test]
    async fn a_turn_claim_resolves_once() {
        let claim = DeferredTurnClaim::new();
        claim.resolve(DeferredTurnDisposition::Started);
        claim.resolve(DeferredTurnDisposition::Delegated);
        assert_eq!(claim.disposition().await, Some(DeferredTurnDisposition::Started));
    }
}
