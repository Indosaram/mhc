//! Port of senpi packages/ai/src/tool-call-middleware/recovery-event-stream.ts.
//! Rust's borrow checker forbids overriding `AssistantMessageEventStream::push` on a
//! plain wrapper, so callers snapshot each event through `snapshot_recovery_event`
//! before pushing; `RecoveryCancellation` tracks the one-shot cancellation lifecycle
//! that the TS class attaches to its overridden async iterator's `return()`.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Mutex, OnceLock};

use tokio::sync::watch;

type CancellationHandler = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

pub struct RecoveryCancellation {
    handler: Mutex<Option<CancellationHandler>>,
    cancellation: OnceLock<watch::Receiver<bool>>,
    source_closed: Mutex<bool>,
}

impl Default for RecoveryCancellation {
    fn default() -> Self {
        Self { handler: Mutex::new(None), cancellation: OnceLock::new(), source_closed: Mutex::new(false) }
    }
}

impl RecoveryCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_cancellation_handler(&self, handler: CancellationHandler) {
        *self.handler.lock().unwrap_or_else(|error| error.into_inner()) = Some(handler);
    }

    pub fn mark_source_closed(&self) -> bool {
        let mut closed = self.source_closed.lock().unwrap_or_else(|error| error.into_inner());
        if self.cancellation.get().is_some() {
            return false;
        }
        *closed = true;
        true
    }

    pub async fn cancel(&self) {
        let mut completion = {
            let closed = self.source_closed.lock().unwrap_or_else(|error| error.into_inner());
            if *closed { return; }
            self.cancellation.get_or_init(|| {
                let handler = self.handler.lock().unwrap_or_else(|error| error.into_inner()).take();
                let (completed, completion) = watch::channel(false);
                tokio::spawn(async move {
                    if let Some(handler) = handler { handler().await; }
                    completed.send_replace(true);
                });
                completion
            }).clone()
        };
        loop {
            if *completion.borrow() { return; }
            if completion.changed().await.is_err() { return; }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use super::*;

    #[tokio::test]
    async fn concurrent_cancellation_survives_a_dropped_waiter() {
        let cancellation = RecoveryCancellation::new();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        cancellation.set_cancellation_handler(Box::new(move || Box::pin(async move {
            entered_tx.send(()).expect("entered subscriber");
            release_rx.await.expect("release");
        })));
        let bound = std::time::Duration::from_secs(5);
        let mut first = Box::pin(cancellation.cancel());
        assert!(futures::poll!(first.as_mut()).is_pending());
        tokio::time::timeout(bound, entered_rx).await.expect("entered deadline").expect("entered");
        assert!(!cancellation.mark_source_closed());
        drop(first);
        let mut second = Box::pin(cancellation.cancel());
        assert!(futures::poll!(second.as_mut()).is_pending());
        release_tx.send(()).expect("release subscriber");
        tokio::time::timeout(bound, second).await.expect("cancel deadline");
    }

    #[tokio::test]
    async fn cancel_invokes_the_handler_exactly_once() {
        let cancellation = RecoveryCancellation::new();
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();
        cancellation.set_cancellation_handler(Box::new(move || {
            Box::pin(async move {
                called_clone.store(true, Ordering::SeqCst);
            })
        }));
        cancellation.cancel().await;
        cancellation.cancel().await;
        assert!(called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn cancel_is_a_no_op_once_the_source_is_marked_closed() {
        let cancellation = RecoveryCancellation::new();
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();
        cancellation.set_cancellation_handler(Box::new(move || {
            Box::pin(async move {
                called_clone.store(true, Ordering::SeqCst);
            })
        }));
        assert!(cancellation.mark_source_closed());
        cancellation.cancel().await;
        assert!(!called.load(Ordering::SeqCst));
    }

    #[test]
    fn mark_source_closed_fails_once_cancellation_has_started() {
        let cancellation = RecoveryCancellation::new();
        let (_completed, completion) = watch::channel(true);
        assert!(cancellation.cancellation.set(completion).is_ok());
        assert!(!cancellation.mark_source_closed());
    }
}
