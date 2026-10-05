use std::thread::JoinHandle;
use maho_ext_api::{AbortSignal, ToolError};

struct OwnedWorker<C: FnOnce() -> Result<(), ToolError>> {
    thread: Option<JoinHandle<()>>,
    cancel: Option<C>,
}

impl<C: FnOnce() -> Result<(), ToolError>> Drop for OwnedWorker<C> {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() && let Err(error) = cancel() {
            eprintln!("dropped task worker cancellation failed: {error}");
        }
        if let Some(thread) = self.thread.take() && thread.join().is_err() {
            eprintln!("dropped task worker panicked");
        }
    }
}

pub(crate) async fn settle<T>(
    receiver: tokio::sync::oneshot::Receiver<T>,
    worker: JoinHandle<()>,
    signal: &AbortSignal,
    cancel: impl FnOnce() -> Result<(), ToolError>,
    panic_message: &str,
) -> Result<T, ToolError> {
    let mut owner = OwnedWorker { thread: Some(worker), cancel: Some(cancel) };
    tokio::pin!(receiver);
    let mut cancellation_error = None;
    let result = tokio::select! {
        result = &mut receiver => result,
        () = signal.cancelled() => {
            if let Some(cancel) = owner.cancel.take() { cancellation_error = cancel().err(); }
            receiver.await
        }
    };
    owner.cancel.take();
    if let Some(worker) = owner.thread.take() {
        worker.join().map_err(|_| ToolError::Message(panic_message.into()))?;
    }
    if let Some(error) = cancellation_error { return Err(error); }
    result.map_err(|error| ToolError::Message(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[tokio::test]
    async fn dropped_pending_future_cancels_and_joins_worker() {
        let (release, released) = mpsc::channel();
        let (finished, completion) = mpsc::channel();
        let (sender, receiver) = tokio::sync::oneshot::channel::<()>();
        let worker = std::thread::spawn(move || {
            released.recv_timeout(Duration::from_secs(5)).expect("drop cancellation");
            drop(sender);
            finished.send(()).expect("completion receiver");
        });
        let signal = AbortSignal::default();
        let mut future = Box::pin(settle(receiver, worker, &signal, || {
            release.send(()).expect("release worker");
            Ok(())
        }, "worker panicked"));
        std::future::poll_fn(|context| {
            assert!(future.as_mut().poll(context).is_pending());
            std::task::Poll::Ready(())
        }).await;
        drop(future);
        completion.try_recv().expect("drop joined worker");
    }

    #[tokio::test]
    async fn cancellation_error_waits_for_worker_settlement() {
        let (release, released) = mpsc::channel();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let (finished, completion) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            released.recv_timeout(Duration::from_secs(5)).expect("cancel signal");
            sender.send(()).expect("receiver retained until settlement");
            finished.send(()).expect("completion receiver");
        });
        let signal = AbortSignal::default();
        signal.abort();
        let result = settle(receiver, worker, &signal, || {
            release.send(()).expect("release worker");
            Err(ToolError::Message("cancel persistence failed".into()))
        }, "worker panicked").await;
        assert!(matches!(result, Err(ToolError::Message(message)) if message == "cancel persistence failed"));
        // A successful join, rather than scheduling luck after send, makes this ready.
        completion.try_recv().expect("worker joined before error returned");
    }

    #[tokio::test]
    async fn closed_receiver_joins_panicked_worker() {
        let (sender, receiver) = tokio::sync::oneshot::channel::<()>();
        let worker = std::thread::spawn(move || {
            drop(sender);
            panic!("worker failure after receiver closure");
        });
        let result = settle(receiver, worker, &AbortSignal::default(), || Ok(()), "worker panicked").await;
        assert!(matches!(result, Err(ToolError::Message(message)) if message == "worker panicked"));
    }
}
