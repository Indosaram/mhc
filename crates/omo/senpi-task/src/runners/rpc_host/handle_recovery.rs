//! Recovery-bound abandonment belongs to the attempt, not the transport.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[derive(Clone, Default)]
pub struct HandleRecovery {
    abandoned: Arc<AtomicBool>,
}
impl HandleRecovery {
    pub fn abandon(&self) {
        self.abandoned.store(true, Ordering::Release);
    }
    pub fn abandoned(&self) -> bool {
        self.abandoned.load(Ordering::Acquire)
    }
    pub fn bounded_reattach<T: Send + 'static>(
        &self,
        budget: std::time::Duration,
        reattach: impl FnOnce(Arc<dyn Fn() -> bool + Send + Sync>) -> T + Send + 'static,
    ) -> Option<T> {
        let state = self.clone();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let deadline = std::time::Instant::now() + budget;
        std::thread::spawn(move || {
            let predicate =
                Arc::new(move || state.abandoned() || std::time::Instant::now() >= deadline);
            let result = reattach(predicate);
            let _ = tx.send(result);
        });
        match rx.recv_timeout(budget) {
            Ok(result) => Some(result),
            Err(_) => {
                self.abandon();
                None
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn abandoning_recovery_is_visible_to_retained_attempts() {
        let state = HandleRecovery::default();
        let observed = state.clone();
        state.abandon();
        assert!(observed.abandoned());
    }
}
