//! Port of `packages/omo-senpi/src/components/telemetry/omo-native-summary-wiring.ts`.
//!
//! The capture facade the recorder publishes through: an ack-gated `session_summary` leaves through the
//! privacy client's immediate send, so the pending file is retired only once the server accepted that very
//! event. The raw-turn sample rate is resolved lazily from config, env and sink.

pub type FlushCallback = Mutex<Option<Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>>;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::{Map, Value};
use telemetry_core::{
    EventTelemetryCaptureOptions, TelemetryDeliveryError, TelemetryDiagnostics, TelemetryError,
    TurnTracing,
};

use crate::omo_native_consent::NativeConsent;
use crate::omo_native_session_summary::{SessionSummaryOptions, SessionSummaryRecorder};
use crate::session_role::TelemetrySessionRole;
use crate::session_summary_delivery::SummaryPublication;
use crate::session_summary_liveness::PidProbe;
use crate::session_summary_owner::current_process_owner;
use crate::session_summary_publisher::SummaryPublish;
use crate::session_summary_sampling::{TelemetrySink, resolve_raw_turn_sample_rate};
use crate::session_summary_store::{Probes, create_summary_storage};

pub const SUMMARY_SHUTDOWN_WAIT_MS: u64 = 1_000;

/// The privacy client's capture entry: an ack-gated event returns the transport's immediate-send future.
pub type CaptureFn = Arc<
    dyn Fn(&str, Map<String, Value>, Option<EventTelemetryCaptureOptions>) -> Option<
            BoxFuture<'static, Result<(), TelemetryDeliveryError>>,
        > + Send
        + Sync,
>;

/// What the component's transport facade shares with every registration of one loaded extension.
#[derive(Default)]
pub struct NativeCaptureState {
    pub capture: Mutex<Option<CaptureFn>>,
    pub flush: FlushCallback,
    pub session_hash: Mutex<Option<String>>,
    /// Event uuids the privacy client actually handed to the transport.
    pub delivered: Mutex<BTreeSet<String>>,
    /// The live transport's span sink; unset on the PostHog path, which has none.
    pub traces: Mutex<Option<Arc<dyn TurnTracing>>>,
    /// The one consent gate as of now; unset until registration, so nothing leaves the fan-in before it.
    pub is_consented: Mutex<Option<Arc<dyn Fn() -> bool + Send + Sync>>>,
    /// The same gate read fresh, at the moment a queued batch would leave the process.
    pub is_send_allowed: Mutex<Option<Arc<dyn Fn() -> bool + Send + Sync>>>,
    /// Empties the native SDK queue without sending it; set while a native transport is live.
    pub drop_queued: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl NativeCaptureState {
    pub fn capture(&self) -> Option<CaptureFn> {
        self.capture
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn set_capture(&self, capture: CaptureFn) {
        *self.capture.lock().unwrap_or_else(PoisonError::into_inner) = Some(capture);
    }

    pub fn clear_capture(&self) {
        *self.capture.lock().unwrap_or_else(PoisonError::into_inner) = None;
        *self.flush.lock().unwrap_or_else(PoisonError::into_inner) = None;
        *self
            .session_hash
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }

    pub fn is_consented(&self) -> bool {
        self.is_consented
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|gate| gate())
    }

    pub fn is_send_allowed(&self) -> bool {
        self.is_send_allowed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|gate| gate())
    }

    pub fn traces(&self) -> Option<Arc<dyn TurnTracing>> {
        self.traces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Acknowledged only when the server accepted this very event: the immediate send resolved on a 2xx
    /// answer; a rejected send propagates so the pending file stays.
    pub fn publish(state: &Arc<Self>) -> SummaryPublish {
        let state = Arc::clone(state);
        Arc::new(move |publication: SummaryPublication| {
            let state = Arc::clone(&state);
            async move {
                let Some(capture) = state.capture() else {
                    return Err(transient("the capture facade is not installed"));
                };
                if !state.is_send_allowed() {
                    return Err(transient("consent was withdrawn before the summary left"));
                }
                let options = EventTelemetryCaptureOptions {
                    uuid: Some(publication.uuid.clone()),
                    timestamp_ms: Some(publication.timestamp_ms),
                    ..Default::default()
                };
                let Some(send) =
                    capture("session_summary", publication.properties.clone(), Some(options))
                else {
                    return Err(transient(
                        "session_summary was rejected before reaching the transport",
                    ));
                };
                send.await
            }
            .boxed()
        })
    }
}

fn transient(message: &str) -> TelemetryDeliveryError {
    TelemetryDeliveryError::new(None, Some(TelemetryError::new(message)))
}

pub struct NativeSummaryRecorderInput {
    pub state: Arc<NativeCaptureState>,
    pub consent: Arc<NativeConsent>,
    pub state_dir: PathBuf,
    pub env: telemetry_core::TelemetryEnv,
    pub cwd: Option<PathBuf>,
    pub session_role: TelemetrySessionRole,
    /// Share of sessions that also send every raw `turn_completed`; tests pin it to 0 or 1.
    pub raw_turn_sample_rate: Option<f64>,
    pub is_pid_alive: Option<PidProbe>,
    pub diagnostics: Option<TelemetryDiagnostics>,
}

pub fn create_native_summary_recorder(
    input: &NativeSummaryRecorderInput,
) -> Arc<SessionSummaryRecorder> {
    let resolved = Arc::new(Mutex::new(input.raw_turn_sample_rate));
    let configured_cwd = input.cwd.clone();
    let env = input.env.clone();
    let sample_rate: Arc<dyn Fn() -> f64 + Send + Sync> = Arc::new(move || {
        let mut cached = resolved.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(rate) = *cached {
            return rate;
        }
        let configured = configured_sample_rate(configured_cwd.as_deref(), &env);
        let rate = resolve_raw_turn_sample_rate(TelemetrySink::Posthog, configured, &env);
        *cached = Some(rate);
        rate
    });
    let consent_is_consented = Arc::clone(&input.consent);
    let consent_scope = Arc::clone(&input.consent);
    let consent_scope_consented = Arc::clone(&input.consent);
    let storage = create_summary_storage(
        &input.state_dir,
        current_process_owner(),
        Probes {
            is_pid_alive: input.is_pid_alive.clone(),
            process_start_time: None,
        },
        None,
    );
    let diagnostics = input.diagnostics.clone();
    SessionSummaryRecorder::new(SessionSummaryOptions {
        is_consented: Arc::new(move |fresh| consent_is_consented.is_consented(fresh)),
        consent_scope: Arc::new(move || consent_scope.consent_scope()),
        is_scope_consented: Arc::new(move |scope| consent_scope_consented.is_scope_consented(scope)),
        storage,
        publish: NativeCaptureState::publish(&input.state),
        session_role: input.session_role,
        sample_rate,
        now: None,
        diagnostics,
        dropped_events: None,
        is_pid_alive: input.is_pid_alive.clone(),
    })
}

/// `telemetry.rawTurnSampleRate` from omo.json; an unreadable config leaves the documented default.
fn configured_sample_rate(cwd: Option<&Path>, env: &telemetry_core::TelemetryEnv) -> Option<f64> {
    let loaded = maho_omo_config_resolution::load_senpi_omo_config(
        omo_config_core::LoadOmoConfigOptions {
            cwd: cwd.map(|cwd| cwd.to_string_lossy().into_owned()),
            env: Some(env.iter().map(|(key, value)| (key.clone(), value.clone())).collect()),
            ..Default::default()
        },
    );
    omo_config_core::omo_telemetry_raw_turn_sample_rate(&loaded.config)
}

/// Waits for `work`, giving up after `ms`: a shutdown never blocks the host on a transport.
pub async fn settle_within(work: BoxFuture<'static, ()>, ms: u64) {
    let timeout = tokio::time::sleep(std::time::Duration::from_millis(ms));
    futures::future::select(work, Box::pin(timeout)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex as StdMutex;

    fn options(enabled: Arc<AtomicBool>) -> crate::index::SenpiTelemetryOptions {
        crate::index::SenpiTelemetryOptions {
            env: Some(
                [("POSTHOG_API_KEY".to_string(), "fixture-key".to_string())]
                    .into_iter()
                    .collect(),
            ),
            is_config_enabled: Some(Arc::new(move |_| enabled.load(Ordering::SeqCst))),
            consent_now: Some(Arc::new(|| 1000.0)),
            ..Default::default()
        }
    }

    fn publication() -> SummaryPublication {
        SummaryPublication {
            uuid: "01234567-89ab-cdef-0123-456789abcdef".into(),
            properties: Map::from_iter([("turns".to_string(), json!(1))]),
            timestamp_ms: 1000.0,
        }
    }

    #[tokio::test]
    async fn an_acknowledged_publish_resolves_ok_and_carries_the_uuid() {
        let state = Arc::new(NativeCaptureState::default());
        let seen: Arc<StdMutex<Vec<EventTelemetryCaptureOptions>>> = Arc::default();
        let observed = Arc::clone(&seen);
        state.set_capture(Arc::new(move |name, _properties, options| {
            assert_eq!(name, "session_summary");
            if let Some(options) = options {
                observed.lock().unwrap().push(options);
            }
            Some(async { Ok(()) }.boxed())
        }));
        *state.is_send_allowed.lock().unwrap() = Some(Arc::new(|| true));
        let publish = NativeCaptureState::publish(&state);
        assert!((publish)(publication()).await.is_ok());
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].uuid.as_deref(),
            Some("01234567-89ab-cdef-0123-456789abcdef")
        );
        assert_eq!(seen[0].timestamp_ms, Some(1000.0));
    }

    #[tokio::test]
    async fn a_missing_facade_withdrawal_or_rejection_keeps_the_record() {
        let state = Arc::new(NativeCaptureState::default());
        let publish = NativeCaptureState::publish(&state);
        assert!((publish)(publication()).await.is_err());

        state.set_capture(Arc::new(|_, _, _| Some(async { Ok(()) }.boxed())));
        // No send gate installed yet: nothing may leave.
        assert!((publish)(publication()).await.is_err());

        *state.is_send_allowed.lock().unwrap() = Some(Arc::new(|| false));
        assert!((publish)(publication()).await.is_err());

        *state.is_send_allowed.lock().unwrap() = Some(Arc::new(|| true));
        state.set_capture(Arc::new(|_, _, _| None));
        assert!((publish)(publication()).await.is_err());
    }

    #[tokio::test]
    async fn a_rejected_send_propagates_as_transient_or_permanent() {
        let state = Arc::new(NativeCaptureState::default());
        *state.is_send_allowed.lock().unwrap() = Some(Arc::new(|| true));
        state.set_capture(Arc::new(|_, _, _| {
            Some(
                async {
                    Err(TelemetryDeliveryError::new(
                        Some(500),
                        Some(TelemetryError::new("server")),
                    ))
                }
                .boxed(),
            )
        }));
        let publish = NativeCaptureState::publish(&state);
        let error = (publish)(publication()).await.expect_err("rejected");
        assert!(!error.permanent);
        state.set_capture(Arc::new(|_, _, _| {
            Some(
                async { Err(TelemetryDeliveryError::new(Some(400), None)) }.boxed(),
            )
        }));
        assert!((publish)(publication()).await.expect_err("refused").permanent);
    }

    #[tokio::test]
    async fn settle_within_bounds_a_hanging_publication() {
        let hanging: BoxFuture<'static, ()> = Box::pin(futures::future::pending());
        let started = std::time::Instant::now();
        settle_within(hanging, 20).await;
        assert!(started.elapsed() >= std::time::Duration::from_millis(15));
        settle_within(Box::pin(async {}), 1_000).await;
    }

    #[test]
    fn an_opted_out_registration_creates_no_recorder_state() {
        let dir = tempfile::tempdir().unwrap();
        let enabled = Arc::new(AtomicBool::new(false));
        let consent = NativeConsent::new(&options(Arc::clone(&enabled)));
        consent.set_cwd(dir.path());
        // The component only builds the recorder when registration consented; the wiring itself writes
        // nothing until a turn is recorded.
        let state = Arc::new(NativeCaptureState::default());
        let _recorder = create_native_summary_recorder(&NativeSummaryRecorderInput {
            state,
            consent,
            state_dir: dir.path().to_path_buf(),
            env: telemetry_core::TelemetryEnv::new(),
            cwd: Some(dir.path().to_path_buf()),
            session_role: TelemetrySessionRole::Root,
            raw_turn_sample_rate: Some(0.0),
            is_pid_alive: None,
            diagnostics: None,
        });
        assert!(!dir.path().join("session-summary").exists());
    }

    #[test]
    fn the_raw_turn_sample_rate_is_resolved_once() {
        let dir = tempfile::tempdir().unwrap();
        let consent = NativeConsent::new(&options(Arc::new(AtomicBool::new(true))));
        consent.set_cwd(dir.path());
        let recorder = create_native_summary_recorder(&NativeSummaryRecorderInput {
            state: Arc::new(NativeCaptureState::default()),
            consent,
            state_dir: dir.path().to_path_buf(),
            env: telemetry_core::TelemetryEnv::new(),
            cwd: Some(dir.path().to_path_buf()),
            session_role: TelemetrySessionRole::Root,
            raw_turn_sample_rate: Some(0.25),
            is_pid_alive: None,
            diagnostics: None,
        });
        assert_eq!(recorder.sample_rate(), 0.25);
        assert_eq!(recorder.sample_rate(), 0.25);
    }
}
