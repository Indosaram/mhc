//! Rust port of `packages/telemetry-core/src/otlp-sleep.ts`.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::sync::Notify;

#[derive(Default)]
struct AbortState {
    aborted: AtomicBool,
    notify: Notify,
}

/// The `AbortSignal` the retry loop and the sender share.
#[derive(Clone, Default)]
pub struct AbortSignal {
    state: Arc<AbortState>,
}

impl AbortSignal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn abort(&self) {
        self.state.aborted.store(true, Ordering::SeqCst);
        self.state.notify.notify_waiters();
    }

    pub fn is_aborted(&self) -> bool {
        self.state.aborted.load(Ordering::SeqCst)
    }

    /// Resolves when the signal aborts; `Notify` keeps a permit, so an abort before this call still
    /// resolves it.
    pub async fn cancelled(&self) {
        if self.is_aborted() {
            return;
        }
        self.state.notify.notified().await;
    }
}

pub type SleepFn = Arc<dyn Fn(u64, AbortSignal) -> BoxFuture<'static, ()> + Send + Sync>;

/// The retry pause; it ends early when the sender aborts.
pub fn abortable_sleep(ms: u64, abort: AbortSignal) -> BoxFuture<'static, ()> {
    async move {
        if abort.is_aborted() {
            return;
        }
        let sleep = tokio::time::sleep(Duration::from_millis(ms));
        tokio::select! {
            () = sleep => {}
            () = abort.cancelled() => {}
        }
    }
    .boxed()
}

/// A cancellable repeating task, standing in for the unref'd `setInterval` of the TS runtime.
pub struct CancelHandle {
    handle: tokio::task::JoinHandle<()>,
}

impl CancelHandle {
    pub fn cancel(self) {
        self.handle.abort();
    }
}

/// Runs `run` every `ms`; `None` when there is no Tokio runtime to schedule on, like the TS timers
/// that never keep the host alive.
pub fn every_unref<F, Fut>(run: F, ms: u64) -> Option<CancelHandle>
where
    F: Fn() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let _handle = tokio::runtime::Handle::try_current().ok()?;
    let period = Duration::from_millis(ms.max(1));
    let handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(period);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            run().await;
        }
    });
    Some(CancelHandle { handle })
}

/// A one-shot delay whose pending timer can be cancelled.
pub struct Delay {
    receiver: tokio::sync::oneshot::Receiver<()>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl Delay {
    pub async fn elapsed(&mut self) {
        let _received = (&mut self.receiver).await;
    }

    pub fn cancel(mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

pub fn delay(ms: u64) -> Delay {
    let (sender, receiver) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        let _sent = sender.send(());
    });
    Delay {
        receiver,
        handle: Some(handle),
    }
}
