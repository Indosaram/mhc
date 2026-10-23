//! Port of senpi extensions/session-start-turn-gate.ts: holds turns that extensions request while
//! `session_start` handlers are still dispatching and starts them once every handler has returned.
use maho_ext_api::ExtensionFailure;
use std::sync::{Arc, Mutex, PoisonError};

pub type TurnStart = Box<dyn FnOnce() -> Result<(), ExtensionFailure> + Send>;
type StartError = Arc<dyn Fn(ExtensionFailure) + Send + Sync>;

pub struct SessionStartTurnGate {
    held: Mutex<Option<Vec<TurnStart>>>,
    on_start_error: StartError,
}
impl SessionStartTurnGate {
    pub fn new(on_start_error: StartError) -> Self {
        Self {
            held: Mutex::new(None),
            on_start_error,
        }
    }
    /// True while `session_start` handlers run: turns they trigger start once dispatch ends.
    pub fn holding(&self) -> bool {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }
    /// Holds `start` while dispatching, else runs it now and returns its result.
    ///
    /// # Errors
    /// The start's own failure, only when it ran immediately.
    pub fn admit(&self, start: TurnStart) -> Result<(), ExtensionFailure> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(queue) = held.as_mut() {
            queue.push(start);
            return Ok(());
        }
        drop(held);
        start()
    }
    pub async fn dispatch<T>(&self, run: impl std::future::Future<Output = T>) -> T {
        let nested = {
            let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
            let nested = held.is_some();
            if !nested {
                *held = Some(Vec::new());
            }
            nested
        };
        if nested {
            return run.await;
        }
        struct Release<'a>(&'a SessionStartTurnGate);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                let queue = self
                    .0
                    .held
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
                    .unwrap_or_default();
                for start in queue {
                    if let Err(error) = start() {
                        (self.0.on_start_error)(error);
                    }
                }
            }
        }
        let _release = Release(self);
        run.await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn turns_are_held_until_dispatch_ends() {
        let gate = Arc::new(SessionStartTurnGate::new(Arc::new(|_| {})));
        let started = Arc::new(AtomicUsize::new(0));
        assert!(!gate.holding());
        let (inner_gate, counter) = (Arc::clone(&gate), Arc::clone(&started));
        gate.dispatch(async move {
            assert!(inner_gate.holding());
            let held = Arc::clone(&counter);
            let admitted = inner_gate.admit(Box::new(move || {
                held.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }));
            assert!(admitted.is_ok());
            assert_eq!(counter.load(Ordering::SeqCst), 0);
        })
        .await;
        assert_eq!(started.load(Ordering::SeqCst), 1);
        assert!(!gate.holding());
        let immediate = Arc::clone(&started);
        assert!(
            gate.admit(Box::new(move || {
                immediate.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }))
            .is_ok()
        );
        assert_eq!(started.load(Ordering::SeqCst), 2);
    }
}
