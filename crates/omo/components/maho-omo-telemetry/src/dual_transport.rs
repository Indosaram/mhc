//! Port of the `withOtlpTransport` half of `packages/omo-senpi/src/components/telemetry/lazy-posthog.ts`.
//!
//! Fans every capture to PostHog and, for the events the collector accepts, to a second OTLP transport.
//! Neither branch can hold up the other: the OTLP branch is loaded on the first OTLP event, a failed load
//! backs off for `IMPORT_BACKOFF_MS`, a `None` load is a permanent refusal, and flush/shutdown of the OTLP
//! side are bounded. Only a PostHog flush failure is returned (the consent-withdrawal drain must keep its
//! gate closed when PostHog did not drain its queue). PX-DIVERGENCE: the TS sidecar import is asynchronous
//! (queued captures, a startup bound, disposal of a late import); the Rust OTLP transport is linked in and
//! built synchronously, so those three mechanisms have nothing to guard and are not ported.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use telemetry_core::{
    TelemetryCaptureMessage, TelemetryDeliveryError, TelemetryDiagnosticEvent,
    TelemetryDiagnosticInput, TelemetryDiagnostics, TelemetryError, TelemetryTransport,
    TurnSpanStart, TurnTracing,
};

pub const IMPORT_BACKOFF_MS: u64 = 60_000;
const DEFAULT_TIMEOUT_MS: u64 = 1_000;

pub type LoadOtlp = Arc<
    dyn Fn(&TelemetryCaptureMessage) -> Result<Option<Arc<dyn TelemetryTransport>>, TelemetryError>
        + Send
        + Sync,
>;
pub type IsOtlpEvent = Arc<dyn Fn(&str) -> bool + Send + Sync>;
pub type Consent = Arc<dyn Fn() -> bool + Send + Sync>;

#[derive(Clone)]
pub struct DualTransportInput {
    pub load_otlp: LoadOtlp,
    /// Events the OTLP side can carry; others never start a load, so a `None` load is a final refusal.
    pub is_otlp_event: Option<IsOtlpEvent>,
    pub consent: Option<Consent>,
    pub timeout_ms: Option<u64>,
    pub diagnostics: Option<TelemetryDiagnostics>,
    pub source: Option<String>,
    pub now: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
}

#[derive(Default)]
struct Inner {
    target: Option<Arc<dyn TelemetryTransport>>,
    retry_at: u64,
    closed: bool,
}

pub struct DualTransport {
    posthog: Arc<dyn TelemetryTransport>,
    input: DualTransportInput,
    inner: Arc<Mutex<Inner>>,
    traces: DualTraces,
}

/// `traces.startTurn` of the OTLP target, or nothing while there is none.
struct DualTraces {
    inner: Arc<Mutex<Inner>>,
}

impl TurnTracing for DualTraces {
    fn start_turn(&self, start: TurnSpanStart) -> Option<telemetry_core::TurnSpanHandle> {
        let target = self
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .target
            .clone()?;
        target.traces()?.start_turn(start)
    }
}

impl DualTransport {
    pub fn new(posthog: Box<dyn TelemetryTransport>, input: DualTransportInput) -> Self {
        let inner = Arc::new(Mutex::new(Inner::default()));
        Self {
            posthog: posthog.into(),
            input,
            traces: DualTraces {
                inner: Arc::clone(&inner),
            },
            inner,
        }
    }

    fn now(&self) -> u64 {
        match &self.input.now {
            Some(now) => now(),
            None => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_millis() as u64),
        }
    }

    fn report(&self, error: TelemetryError) {
        if let Some(diagnostics) = &self.input.diagnostics {
            diagnostics(&TelemetryDiagnosticInput::failure(
                TelemetryDiagnosticEvent::TelemetryOtlpSendFailed,
                self.input.source.as_deref().unwrap_or("omo-telemetry"),
                error,
            ));
        }
    }

    /// A consent callback that panics counts as no consent.
    fn allowed(&self) -> bool {
        match &self.input.consent {
            None => true,
            Some(consent) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| consent()))
                .unwrap_or(false),
        }
    }

    fn forward(&self, target: &Arc<dyn TelemetryTransport>, message: &TelemetryCaptureMessage) {
        if !self.allowed() {
            return;
        }
        if let Err(error) = target.capture(message) {
            self.report(error);
        }
    }

    fn capture_otlp(&self, message: &TelemetryCaptureMessage) {
        if self
            .input
            .is_otlp_event
            .as_ref()
            .is_some_and(|is_otlp_event| !is_otlp_event(&message.event))
            || !self.allowed()
        {
            return;
        }
        let target = {
            let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            if inner.closed {
                return;
            }
            if inner.target.is_none() {
                let now = self.now();
                if now < inner.retry_at {
                    return;
                }
                match (self.input.load_otlp)(message) {
                    Ok(Some(loaded)) => inner.target = Some(loaded),
                    // The adapter's settled refusal (no key, unsupported OS): refuse once, not every minute.
                    Ok(None) => inner.retry_at = u64::MAX,
                    Err(error) => {
                        inner.retry_at = inner.retry_at.max(now.saturating_add(IMPORT_BACKOFF_MS));
                        drop(inner);
                        self.report(error);
                        return;
                    }
                }
            }
            inner.target.clone()
        };
        if let Some(target) = target {
            self.forward(&target, message);
        }
    }

    fn timeout(&self) -> Duration {
        Duration::from_millis(self.input.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS))
    }

    async fn finish(&self, shutdown: bool) -> Result<(), TelemetryError> {
        let target = {
            let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            if shutdown {
                inner.closed = true;
            }
            inner.target.clone()
        };
        let posthog = async {
            if shutdown {
                self.posthog.shutdown().await
            } else {
                match self.posthog.flush() {
                    Some(flush) => flush.await,
                    None => Ok(()),
                }
            }
        };
        let otlp = async {
            let Some(target) = target else { return };
            let work = async {
                if shutdown {
                    let _ = target.shutdown().await;
                } else if let Some(flush) = target.flush() {
                    let _ = flush.await;
                }
            };
            let _ = tokio::time::timeout(self.timeout(), work).await;
        };
        let (posthog_result, ()) = futures::join!(posthog, otlp);
        if shutdown { Ok(()) } else { posthog_result }
    }
}

impl TelemetryTransport for DualTransport {
    fn capture(&self, message: &TelemetryCaptureMessage) -> Result<(), TelemetryError> {
        self.capture_otlp(message);
        self.posthog.capture(message)
    }

    /// Pending session-summary files keep the PostHog acknowledgment contract: PostHog answers.
    fn capture_immediate<'a>(
        &'a self,
        message: &'a TelemetryCaptureMessage,
    ) -> Option<BoxFuture<'a, Result<(), TelemetryDeliveryError>>> {
        self.capture_otlp(message);
        Some(match self.posthog.capture_immediate(message) {
            Some(send) => send,
            None => async {
                Err(TelemetryDeliveryError::new(
                    None,
                    Some(TelemetryError::new(
                        "PostHog immediate capture is unavailable",
                    )),
                ))
            }
            .boxed(),
        })
    }

    fn capture_durable(
        &self,
        message: TelemetryCaptureMessage,
    ) -> BoxFuture<'_, Result<(), TelemetryError>> {
        self.capture_otlp(&message);
        self.posthog.capture_durable(message)
    }

    fn traces(&self) -> Option<&dyn TurnTracing> {
        Some(&self.traces)
    }

    fn flush(&self) -> Option<BoxFuture<'_, Result<(), TelemetryError>>> {
        Some(self.finish(false).boxed())
    }

    fn shutdown(&self) -> BoxFuture<'_, Result<(), TelemetryError>> {
        self.finish(true).boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    #[derive(Default)]
    struct Recorder {
        events: Mutex<Vec<String>>,
        fail_flush: bool,
    }
    impl TelemetryTransport for Recorder {
        fn capture(&self, message: &TelemetryCaptureMessage) -> Result<(), TelemetryError> {
            self.events
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(message.event.clone());
            Ok(())
        }
        fn flush(&self) -> Option<BoxFuture<'_, Result<(), TelemetryError>>> {
            let fail = self.fail_flush;
            Some(
                async move {
                    if fail {
                        Err(TelemetryError::new("flush failed"))
                    } else {
                        Ok(())
                    }
                }
                .boxed(),
            )
        }
        fn shutdown(&self) -> BoxFuture<'_, Result<(), TelemetryError>> {
            async { Ok(()) }.boxed()
        }
    }

    fn message(event: &str) -> TelemetryCaptureMessage {
        TelemetryCaptureMessage {
            uuid: None,
            timestamp: None,
            distinct_id: "d".into(),
            event: event.into(),
            properties: Default::default(),
        }
    }

    struct Harness {
        dual: DualTransport,
        posthog: Arc<Recorder>,
        otlp: Arc<Recorder>,
        loads: Arc<AtomicUsize>,
        clock: Arc<AtomicU64>,
    }

    struct Shared(Arc<Recorder>);
    impl TelemetryTransport for Shared {
        fn capture(&self, message: &TelemetryCaptureMessage) -> Result<(), TelemetryError> {
            self.0.capture(message)
        }
        fn flush(&self) -> Option<BoxFuture<'_, Result<(), TelemetryError>>> {
            self.0.flush()
        }
        fn shutdown(&self) -> BoxFuture<'_, Result<(), TelemetryError>> {
            self.0.shutdown()
        }
    }

    fn harness(
        fail_posthog_flush: bool,
        outcome: impl Fn(usize) -> Result<bool, TelemetryError> + Send + Sync + 'static,
        consent: Option<Consent>,
    ) -> Harness {
        let posthog = Arc::new(Recorder {
            fail_flush: fail_posthog_flush,
            ..Default::default()
        });
        let otlp = Arc::new(Recorder::default());
        let loads = Arc::new(AtomicUsize::new(0));
        let clock = Arc::new(AtomicU64::new(0));
        let (loaded, counter) = (Arc::clone(&otlp), Arc::clone(&loads));
        let load_otlp: LoadOtlp = Arc::new(move |_| {
            let attempt = counter.fetch_add(1, Ordering::SeqCst);
            Ok(outcome(attempt)?
                .then(|| Arc::new(Shared(Arc::clone(&loaded))) as Arc<dyn TelemetryTransport>))
        });
        let tick = Arc::clone(&clock);
        let dual = DualTransport::new(
            Box::new(Shared(Arc::clone(&posthog))),
            DualTransportInput {
                load_otlp,
                is_otlp_event: Some(Arc::new(|event| event != "legacy_only")),
                consent,
                timeout_ms: None,
                diagnostics: None,
                source: None,
                now: Some(Arc::new(move || tick.load(Ordering::SeqCst))),
            },
        );
        Harness {
            dual,
            posthog,
            otlp,
            loads,
            clock,
        }
    }

    fn seen(recorder: &Recorder) -> Vec<String> {
        recorder
            .events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    #[test]
    fn fans_out_otlp_events_and_keeps_legacy_only_events_on_posthog() {
        let h = harness(false, |_| Ok(true), None);
        h.dual.capture(&message("session_started")).unwrap();
        h.dual.capture(&message("legacy_only")).unwrap();
        assert_eq!(seen(&h.posthog), ["session_started", "legacy_only"]);
        assert_eq!(seen(&h.otlp), ["session_started"]);
    }

    #[test]
    fn null_load_is_a_permanent_refusal_and_failed_load_backs_off() {
        let refused = harness(false, |_| Ok(false), None);
        refused.dual.capture(&message("a")).unwrap();
        refused
            .clock
            .store(10 * IMPORT_BACKOFF_MS, Ordering::SeqCst);
        refused.dual.capture(&message("a")).unwrap();
        assert_eq!(refused.loads.load(Ordering::SeqCst), 1);

        let flaky = harness(
            false,
            |attempt| {
                if attempt == 0 {
                    Err(TelemetryError::new("boom"))
                } else {
                    Ok(true)
                }
            },
            None,
        );
        flaky.dual.capture(&message("a")).unwrap();
        flaky.dual.capture(&message("b")).unwrap();
        assert_eq!(flaky.loads.load(Ordering::SeqCst), 1);
        flaky.clock.store(IMPORT_BACKOFF_MS, Ordering::SeqCst);
        flaky.dual.capture(&message("c")).unwrap();
        assert_eq!(seen(&flaky.otlp), ["c"]);
        assert_eq!(seen(&flaky.posthog), ["a", "b", "c"]);
    }

    #[test]
    fn consent_is_read_before_every_otlp_forward() {
        let allowed = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let gate = Arc::clone(&allowed);
        let h = harness(
            false,
            |_| Ok(true),
            Some(Arc::new(move || gate.load(Ordering::SeqCst))),
        );
        h.dual.capture(&message("a")).unwrap();
        allowed.store(false, Ordering::SeqCst);
        h.dual.capture(&message("b")).unwrap();
        assert_eq!(seen(&h.otlp), ["a"]);
        let panicking = harness(false, |_| Ok(true), Some(Arc::new(|| panic!("no consent"))));
        panicking.dual.capture(&message("a")).unwrap();
        assert!(seen(&panicking.otlp).is_empty());
    }

    #[tokio::test]
    async fn flush_rethrows_only_the_posthog_failure_and_shutdown_closes_otlp() {
        let h = harness(true, |_| Ok(true), None);
        h.dual.capture(&message("a")).unwrap();
        assert!(h.dual.flush().unwrap().await.is_err());
        assert!(h.dual.shutdown().await.is_ok());
        h.dual.capture(&message("b")).unwrap();
        assert_eq!(seen(&h.otlp), ["a"]);
    }
}
