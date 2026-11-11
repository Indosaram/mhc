//! Cancellation for a facts run: one flag every in-flight step can observe.
//!
//! Spec: `docs/cleanroom/specs/memory-facts-abort.md`. A cancelled run must stop promptly, and a step that
//! subscribes *after* the abort must observe it immediately rather than waiting for a signal that already
//! fired — otherwise cancelling during startup hangs until the run finishes on its own.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct AbortState {
    aborted: Arc<AtomicBool>,
    subscribers: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
}

impl std::fmt::Debug for AbortState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AbortState")
            .field("aborted", &self.aborted.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default)]
pub struct AbortSignal {
    state: Arc<AbortState>,
}

impl AbortSignal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn abort(&self) {
        if self.state.aborted.swap(true, Ordering::SeqCst) {
            return;
        }
        let subscribers = {
            let mut guard = self
                .state
                .subscribers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut *guard)
        };
        for subscriber in subscribers {
            subscriber();
        }
    }

    pub fn is_aborted(&self) -> bool {
        self.state.aborted.load(Ordering::SeqCst)
    }

    pub fn flag(&self) -> Arc<AtomicBool> {
        self.state.aborted.clone()
    }

    /// Runs `on_abort` when the signal fires, or immediately when it already has.
    ///
    /// The abort flag is re-checked under the lock, so an abort racing the registration cannot be missed: the
    /// two orderings are "subscriber sees the flag" or "abort sees the subscriber", never neither.
    pub fn on_abort(&self, on_abort: impl FnOnce() + Send + 'static) {
        let mut slot = Some(on_abort);
        {
            let mut guard = self
                .state
                .subscribers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !self.state.aborted.load(Ordering::SeqCst) {
                if let Some(callback) = slot.take() {
                    guard.push(Box::new(callback) as Box<dyn FnOnce() + Send>);
                }
                return;
            }
        }
        if let Some(callback) = slot.take() {
            callback();
        }
    }

    /// Resolves when the signal fires; resolves at once when it already has.
    pub async fn aborted(&self) {
        let (sender, receiver) = AbortWake::pair();
        self.on_abort(move || sender.wake());
        receiver.await;
    }
}

#[derive(Debug, Default)]
struct AbortWake {
    woken: AtomicBool,
    waker: Mutex<Option<std::task::Waker>>,
}

impl AbortWake {
    fn pair() -> (Arc<AbortSender>, AbortWaiter) {
        let shared = Arc::new(Self::default());
        (
            Arc::new(AbortSender {
                shared: shared.clone(),
            }),
            AbortWaiter { shared },
        )
    }

    fn wake(&self) {
        self.woken.store(true, Ordering::SeqCst);
        let waker = self
            .waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

#[derive(Debug)]
struct AbortSender {
    shared: Arc<AbortWake>,
}

impl AbortSender {
    fn wake(&self) {
        self.shared.wake();
    }
}

#[derive(Debug)]
struct AbortWaiter {
    shared: Arc<AbortWake>,
}

impl std::future::Future for AbortWaiter {
    type Output = ();

    fn poll(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if self.shared.woken.load(Ordering::SeqCst) {
            return std::task::Poll::Ready(());
        }
        let mut slot = self
            .shared
            .waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.shared.woken.load(Ordering::SeqCst) {
            return std::task::Poll::Ready(());
        }
        let waker = context.waker();
        let replaced = match slot.as_ref() {
            Some(existing) => !existing.will_wake(waker),
            None => true,
        };
        if replaced {
            *slot = Some(waker.clone());
        }
        std::task::Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn a_new_signal_is_not_aborted() {
        assert!(!AbortSignal::new().is_aborted());
    }

    #[test]
    fn aborting_sets_the_flag() {
        let signal = AbortSignal::new();
        signal.abort();
        assert!(signal.is_aborted());
    }

    #[test]
    fn a_subscriber_registered_before_the_abort_runs_once() {
        let signal = AbortSignal::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let counter = runs.clone();
        signal.on_abort(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(runs.load(Ordering::SeqCst), 0);
        signal.abort();
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_subscriber_registered_after_the_abort_runs_immediately() {
        let signal = AbortSignal::new();
        signal.abort();
        let runs = Arc::new(AtomicUsize::new(0));
        let counter = runs.clone();
        signal.on_abort(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_second_abort_does_not_rerun_subscribers() {
        let signal = AbortSignal::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let counter = runs.clone();
        signal.on_abort(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        signal.abort();
        signal.abort();
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn the_shared_flag_observes_the_abort() {
        let signal = AbortSignal::new();
        let flag = signal.flag();
        assert!(!flag.load(Ordering::SeqCst));
        signal.abort();
        assert!(flag.load(Ordering::SeqCst));
    }

    #[test]
    fn clones_share_one_state() {
        let signal = AbortSignal::new();
        let clone = signal.clone();
        clone.abort();
        assert!(signal.is_aborted());
    }

    fn poll_once(signal: &AbortSignal) -> std::task::Poll<()> {
        use std::future::Future;
        let mut future = Box::pin(signal.aborted());
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        future.as_mut().poll(&mut context)
    }

    #[test]
    fn the_aborted_future_is_pending_before_the_abort() {
        let signal = AbortSignal::new();
        assert_eq!(poll_once(&signal), std::task::Poll::Pending);
    }

    #[test]
    fn the_aborted_future_is_ready_after_the_abort() {
        let signal = AbortSignal::new();
        signal.abort();
        assert_eq!(poll_once(&signal), std::task::Poll::Ready(()));
    }

    #[test]
    fn a_future_created_after_the_abort_completes_immediately() {
        let signal = AbortSignal::new();
        signal.abort();
        assert!(poll_once_ready(signal.aborted()));
    }

    fn poll_once_ready(future: impl std::future::Future<Output = ()>) -> bool {
        let mut future = Box::pin(future);
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        matches!(
            future.as_mut().poll(&mut context),
            std::task::Poll::Ready(())
        )
    }

    #[test]
    fn aborting_while_a_waiter_is_registered_wakes_it() {
        use std::future::Future;
        let signal = AbortSignal::new();
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = wakes.clone();

        struct CountingWaker(Arc<AtomicUsize>);
        impl std::task::Wake for CountingWaker {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let waker = std::task::Waker::from(Arc::new(CountingWaker(counter)));
        let mut context = std::task::Context::from_waker(&waker);
        let mut future = Box::pin(signal.aborted());
        assert_eq!(future.as_mut().poll(&mut context), std::task::Poll::Pending);
        signal.abort();
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        assert_eq!(
            future.as_mut().poll(&mut context),
            std::task::Poll::Ready(())
        );
    }
}
