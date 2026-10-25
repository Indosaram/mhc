//! Rust port of `packages/telemetry-core/src/otlp-gate.ts`.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use crate::otlp_config::is_otlp_telemetry_enabled;
use crate::otlp_transport_types::Consent;
use crate::otlp_transport_types::CreateOtlpTelemetryTransportInput;
use crate::types::TelemetryDiagnosticEvent;
use crate::types::TelemetryDiagnosticInput;
use crate::types::TelemetryError;

/// Whether an OTLP transport may start: no opt-out applies and `consent()` is true. A consent
/// callback that panics counts as no consent.
pub fn is_otlp_transport_allowed(input: &CreateOtlpTelemetryTransportInput) -> bool {
    if !is_otlp_telemetry_enabled(input.env.as_ref(), &input.product_env_prefix) {
        return false;
    }
    safe_consent(input)()
}

/// `consent()` with the gate's rule applied on every call: a callback that panics counts as no consent.
pub fn safe_consent(input: &CreateOtlpTelemetryTransportInput) -> Consent {
    let Some(consent) = &input.consent else {
        return Arc::new(|| true);
    };
    let consent = Arc::clone(consent);
    let diagnostics = input.diagnostics.clone();
    let source = input
        .source
        .clone()
        .unwrap_or_else(|| "otlp".to_string());
    Arc::new(move || match std::panic::catch_unwind(AssertUnwindSafe(|| consent())) {
        Ok(value) => value,
        Err(_) => {
            if let Some(diagnostics) = &diagnostics {
                diagnostics(&TelemetryDiagnosticInput {
                    event: TelemetryDiagnosticEvent::TelemetryConsentFailed,
                    source: source.clone(),
                    error: Some(TelemetryError::new("consent callback failed")),
                    error_kind: Some(crate::types::TelemetryDiagnosticErrorKind::Error),
                    entry_kind: None,
                    error_code: None,
                    count: None,
                });
            }
            false
        }
    })
}
