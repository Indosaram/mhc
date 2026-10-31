//! Rust port of `packages/telemetry-core/src/otlp-entry.ts`.
//!
//! The `otlp` entry of the package. It reads the opt-outs and consent with no protobuf code loaded;
//! only an enabled, consented run builds the sender. A disabled run creates nothing on disk and
//! deletes what earlier consented runs left in the spool.

pub use crate::otlp_config::DEFAULT_OTLP_ENDPOINT;
pub use crate::otlp_config::is_otlp_telemetry_enabled;
pub use crate::otlp_config::is_wire_attribute_key;
pub use crate::otlp_config::resolve_otlp_endpoint;
pub use crate::otlp_inspect::InspectedAttributes;
pub use crate::otlp_inspect::InspectedLogRecord;
pub use crate::otlp_inspect::InspectedRequest;
pub use crate::otlp_inspect::InspectedSpan;
pub use crate::otlp_inspect::inspect_otlp_request;
pub use crate::otlp_sender_core::OtlpTelemetryTransport;
pub use crate::otlp_transport_types::CreateOtlpTelemetryTransportInput;

use crate::otlp_gate::is_otlp_transport_allowed;
use crate::otlp_sender::start_otlp_transport;
use crate::otlp_spool_purge::PurgeOptOutInput;
use crate::otlp_spool_purge::purge_on_opt_out;
pub use crate::otlp_spool_purge::purge_owned_spool;

/// The collector transport, or `None` whenever an opt-out applies or consent is not given.
pub fn create_otlp_telemetry_transport(
    input: CreateOtlpTelemetryTransportInput,
) -> Option<OtlpTelemetryTransport> {
    if !is_otlp_transport_allowed(&input) {
        purge_on_opt_out(&PurgeOptOutInput::from(&input));
        return None;
    }
    start_otlp_transport(input)
}
