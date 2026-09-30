//! Port of senpi packages/ai/src/tool-call-middleware/recovery-event-stream.ts.
//! Rust's borrow checker forbids overriding `AssistantMessageEventStream::push` on a
//! plain wrapper, so callers snapshot each event through `snapshot_recovery_event`
//! before pushing; `RecoveryCancellation` tracks the one-shot cancellation lifecycle
//! that the TS class attaches to its overridden async iterator's `return()`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use tokio::sync::OnceCell;

type CancellationHandler = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

pub struct RecoveryCancellation {
    handler: Mutex<Option<CancellationHandler>>,
    cancellation: OnceCell<()>,
    source_closed: Mutex<bool>,
}

impl Default for RecoveryCancellation {
    fn default() -> Self {
        Self { handler: Mutex::new(None), cancellation: OnceCell::new(), source_closed: Mutex::new(false) }
    }
}

impl RecoveryCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_cancellation_handler(&self, handler: CancellationHandler) {
        *self.handler.lock().expect("cancellation handler mutex is not held across a panic") = Some(handler);
    }

    pub fn mark_source_closed(&self) -> bool {
        if self.cancellation.initialized() {
            return false;
        }
        *self.source_closed.lock().expect("source_closed mutex is not held across a panic") = true;
        true
    }

    pub async fn cancel(&self) {
        let already_closed = *self.source_closed.lock().expect("source_closed mutex is not held across a panic");
        if already_closed {
            return;
        }
        let handler = self.handler.lock().expect("cancellation handler mutex is not held across a panic").take();
        self.cancellation
            .get_or_init(|| async {
                if let Some(handler) = handler {
                    handler().await;
                }
            })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use super::*;

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
        assert!(cancellation.cancellation.set(()).is_ok());
        assert!(!cancellation.mark_source_closed());
    }
}
