//! Port of `packages/omo-senpi/src/components/telemetry/otlp.ts`.
//!
//! The Senpi transport factory: the PostHog factory with an OTLP branch added only when an ingest key
//! resolves (`OMO_TELEMETRY_INGEST_KEY` env or the build constant of the same name). Consent stays the
//! caller's: PostHog sends are gated at send time and the OTLP branch re-reads consent before every forward.

use std::sync::Arc;

use telemetry_core::{
    ConsentGate, PostHogTelemetryTransport, TelemetryCaptureMessage, TelemetryDiagnosticEvent,
    TelemetryDiagnosticInput, TelemetryDiagnostics, TelemetryEnv, TelemetryTransport,
    TelemetryTransportFactory,
};

use crate::dual_transport::{DualTransport, DualTransportInput};
use crate::otlp_events::is_otlp_event;

#[derive(Clone)]
pub struct TransportInput {
    pub env: TelemetryEnv,
    pub consent: Arc<dyn Fn() -> bool + Send + Sync>,
    pub diagnostics: Option<TelemetryDiagnostics>,
    pub source: Option<String>,
}

const BUILD_INGEST_KEY: Option<&str> = option_env!("OMO_TELEMETRY_INGEST_KEY");

fn resolve_ingest_key(env: &TelemetryEnv) -> String {
    env.get("OMO_TELEMETRY_INGEST_KEY")
        .map(String::as_str)
        .or(BUILD_INGEST_KEY)
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Checks the key before anything of the exporter is built; failures stay on the OTLP branch.
/// `None` on an empty key, a non-OTLP event, or an OS outside darwin/linux/win32.
pub fn create_senpi_otlp_transport(
    input: &TransportInput,
    message: &TelemetryCaptureMessage,
) -> Option<Arc<dyn TelemetryTransport>> {
    let ingest_key = resolve_ingest_key(&input.env);
    if ingest_key.is_empty() || !is_otlp_event(&message.event) {
        return None;
    }
    if !cfg!(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "windows"
    )) {
        return None;
    }
    crate::otlp_runtime::create_senpi_otlp_transport(input, &ingest_key, message)
}

pub fn create_senpi_telemetry_transport_factory(
    input: TransportInput,
) -> TelemetryTransportFactory {
    let has_key = !resolve_ingest_key(&input.env).is_empty();
    Arc::new(move |api_key, options| {
        let diagnostics = input.diagnostics.clone();
        let source = input.source.clone();
        let gate = ConsentGate {
            is_send_allowed: Arc::clone(&input.consent),
            on_dropped: diagnostics.map(|diagnostics| {
                Arc::new(move |count: usize| {
                    diagnostics(&TelemetryDiagnosticInput {
                        event: TelemetryDiagnosticEvent::TelemetryEventsDroppedByConsent,
                        source: source.clone().unwrap_or_else(|| "omo-telemetry".into()),
                        error: None,
                        error_kind: None,
                        entry_kind: None,
                        error_code: None,
                        count: Some(count as u64),
                    });
                }) as Arc<dyn Fn(usize) + Send + Sync>
            }),
        };
        let posthog = PostHogTelemetryTransport::new(api_key, options)?.with_consent_gate(gate);
        if !has_key {
            return Ok(Box::new(posthog) as Box<dyn TelemetryTransport>);
        }
        let otlp_input = input.clone();
        Ok(Box::new(DualTransport::new(
            Box::new(posthog),
            DualTransportInput {
                load_otlp: Arc::new(move |message| {
                    Ok(create_senpi_otlp_transport(&otlp_input, message))
                }),
                is_otlp_event: Some(Arc::new(is_otlp_event)),
                consent: Some(Arc::clone(&input.consent)),
                timeout_ms: None,
                diagnostics: input.diagnostics.clone(),
                source: input.source.clone(),
                now: None,
            },
        )) as Box<dyn TelemetryTransport>)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(env: &[(&str, &str)]) -> TransportInput {
        TransportInput {
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            consent: Arc::new(|| true),
            diagnostics: None,
            source: None,
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

    #[test]
    fn refuses_without_a_key_and_for_non_otlp_events() {
        assert!(create_senpi_otlp_transport(&input(&[]), &message("daily_active")).is_none());
        let keyed = input(&[("OMO_TELEMETRY_INGEST_KEY", "  ")]);
        assert!(create_senpi_otlp_transport(&keyed, &message("daily_active")).is_none());
        let keyed = input(&[("OMO_TELEMETRY_INGEST_KEY", "k")]);
        assert!(create_senpi_otlp_transport(&keyed, &message("legacy_event")).is_none());
    }

    #[test]
    fn key_resolves_from_env_and_is_trimmed() {
        assert_eq!(
            resolve_ingest_key(&input(&[("OMO_TELEMETRY_INGEST_KEY", " k ")]).env),
            "k"
        );
    }
}
