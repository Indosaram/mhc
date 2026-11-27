//! senpi `modes/rpc/compact-admission-producer.ts`: bounded producer execution with epoch
//! abort signal, late-result release, and leak-signal loss counters.

use crate::compact_admission_error::{CompactAdmissionError, CompactAdmissionReason};
use crate::rpc_request_deadline::OPEN_AFTER_QUEUED_DEADLINE_MS;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::watch;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProducerLosses {
    pub abandoned: u64,
    pub release_failed: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProducerLossKind {
    Abandoned,
    ReleaseFailed,
}

impl ProducerLossKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Abandoned => "abandoned",
            Self::ReleaseFailed => "releaseFailed",
        }
    }
}

/// Abandoned producers and failed late releases are leak signals: counted and logged, never silent.
pub fn record_producer_loss(
    losses: &mut ProducerLosses,
    kind: ProducerLossKind,
    count: u64,
    cause: Option<&str>,
) {
    if count == 0 {
        return;
    }
    match kind {
        ProducerLossKind::Abandoned => losses.abandoned += count,
        ProducerLossKind::ReleaseFailed => losses.release_failed += count,
    }
    let detail = cause.map_or_else(String::new, |msg| format!(": {msg}"));
    eprintln!(
        "senpi rpc admission producer {} {count}{detail}",
        kind.as_str()
    );
}

#[derive(Clone)]
pub struct ProducerAbortSignal {
    tx: watch::Sender<Option<CompactAdmissionError>>,
}

impl ProducerAbortSignal {
    pub fn is_aborted(&self) -> bool {
        self.tx.borrow().is_some()
    }

    pub fn reason(&self) -> Option<CompactAdmissionError> {
        self.tx.borrow().clone()
    }

    pub async fn aborted(&self) -> CompactAdmissionError {
        let mut rx = self.tx.subscribe();
        loop {
            if let Some(reason) = rx.borrow_and_update().clone() {
                return reason;
            }
            if rx.changed().await.is_err() {
                return CompactAdmissionError::new(CompactAdmissionReason::StorageUnavailable);
            }
        }
    }
}

/// One deadline per ending epoch, shared by all its bounded producers and duplicate teardown calls.
pub struct AdmissionProducerLifetime {
    tx: watch::Sender<Option<CompactAdmissionError>>,
    timer: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Default for AdmissionProducerLifetime {
    fn default() -> Self {
        Self::new()
    }
}

impl AdmissionProducerLifetime {
    pub fn new() -> Self {
        let (tx, _) = watch::channel(None);
        Self {
            tx,
            timer: Mutex::new(None),
        }
    }

    pub fn signal(&self) -> ProducerAbortSignal {
        ProducerAbortSignal {
            tx: self.tx.clone(),
        }
    }

    pub fn abort_now(&self, reason: CompactAdmissionError) {
        self.tx.send_replace(Some(reason));
    }

    pub fn end(&self, abandoned: impl FnOnce() + Send + 'static) {
        self.end_after(
            std::time::Duration::from_millis(OPEN_AFTER_QUEUED_DEADLINE_MS),
            abandoned,
        );
    }

    pub fn end_after(&self, delay: std::time::Duration, abandoned: impl FnOnce() + Send + 'static) {
        let mut guard = self.timer.lock().unwrap_or_else(PoisonError::into_inner);
        if guard.is_some() || tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let tx = self.tx.clone();
        let abandoned = Arc::new(Mutex::new(Some(abandoned)));
        *guard = Some(tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            if let Some(cb) = abandoned
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                cb();
            }
            tx.send_replace(Some(CompactAdmissionError::new(
                CompactAdmissionReason::StorageUnavailable,
            )));
        }));
    }

    pub fn close(&self) {
        if let Some(handle) = self
            .timer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            handle.abort();
        }
    }
}

impl Drop for AdmissionProducerLifetime {
    fn drop(&mut self) {
        self.close();
    }
}

/// Join the operation or its epoch abort, releasing late results without publishing them.
pub async fn run_admission_producer<T, F, Fut, R, RFut>(
    signal: ProducerAbortSignal,
    produce: F,
    release: R,
) -> Result<T, CompactAdmissionError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, CompactAdmissionError>> + Send + 'static,
    R: FnOnce(T) -> RFut + Send + 'static,
    RFut: Future<Output = ()> + Send + 'static,
    T: Send + 'static,
{
    if let Some(reason) = signal.reason() {
        return Err(reason);
    }
    let mut handle = tokio::spawn(produce());
    tokio::select! {
        biased;
        reason = signal.aborted() => {
            // Detach late completion so it releases its produced resource when it finishes.
            let signal_check = signal.clone();
            tokio::spawn(async move {
                if let Ok(Ok(value)) = handle.await
                    && signal_check.is_aborted()
                {
                    release(value).await;
                }
            });
            Err(reason)
        }
        res = &mut handle => {
            match res {
                Ok(Ok(value)) => {
                    if let Some(reason) = signal.reason() {
                        release(value).await;
                        Err(reason)
                    } else {
                        Ok(value)
                    }
                }
                Ok(Err(err)) => Err(err),
                Err(_) => Err(CompactAdmissionError::new(CompactAdmissionReason::StorageUnavailable)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn resolves_produced_value_when_not_aborted() {
        let lifetime = AdmissionProducerLifetime::new();
        let value = run_admission_producer(
            lifetime.signal(),
            || async { Ok::<_, CompactAdmissionError>(42usize) },
            |_| async {},
        )
        .await
        .expect("ok");
        assert_eq!(value, 42);
    }

    #[tokio::test]
    async fn releases_late_result_when_signal_aborts() {
        let lifetime = AdmissionProducerLifetime::new();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
        let (released_tx, released_rx) = tokio::sync::oneshot::channel::<usize>();
        let signal = lifetime.signal();
        let fut = run_admission_producer(
            signal,
            move || async move {
                let _ = entered_tx.send(());
                let _ = gate_rx.await;
                Ok::<_, CompactAdmissionError>(99usize)
            },
            move |value| async move {
                let _ = released_tx.send(value);
            },
        );
        let (res, ()) = tokio::join!(fut, async {
            let _ = entered_rx.await;
            lifetime.abort_now(CompactAdmissionError::new(
                CompactAdmissionReason::StorageUnavailable,
            ));
        });
        assert_eq!(
            res.expect_err("aborted").reason,
            CompactAdmissionReason::StorageUnavailable
        );
        let _ = gate_tx.send(());
        assert_eq!(released_rx.await.expect("released"), 99);
    }

    #[test]
    fn records_producer_losses() {
        let mut losses = ProducerLosses::default();
        let called = Arc::new(AtomicBool::new(false));
        record_producer_loss(&mut losses, ProducerLossKind::Abandoned, 0, None);
        record_producer_loss(&mut losses, ProducerLossKind::Abandoned, 2, Some("timeout"));
        record_producer_loss(&mut losses, ProducerLossKind::ReleaseFailed, 1, None);
        assert_eq!(losses.abandoned, 2);
        assert_eq!(losses.release_failed, 1);
        assert!(!called.load(Ordering::SeqCst));
    }
}
