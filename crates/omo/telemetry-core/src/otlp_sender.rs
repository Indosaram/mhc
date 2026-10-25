//! Rust port of `packages/telemetry-core/src/otlp-sender.ts`.

use crate::otlp_gate::is_otlp_transport_allowed;
use crate::otlp_sender_core::OtlpTelemetryTransport;
use crate::otlp_sender_core::create_otlp_sender;
use crate::otlp_transport_types::CreateOtlpTelemetryTransportInput;

/// The only way to start the OTLP runtime. It checks every opt-out and consent itself: on a disabled
/// run it returns `None`, creates nothing and writes nothing.
pub fn start_otlp_transport(
    input: CreateOtlpTelemetryTransportInput,
) -> Option<OtlpTelemetryTransport> {
    if !is_otlp_transport_allowed(&input) {
        return None;
    }
    Some(create_otlp_sender(input))
}
