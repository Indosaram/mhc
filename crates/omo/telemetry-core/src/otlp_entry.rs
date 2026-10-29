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

use std::io;
use std::path::Path;

use crate::otlp_gate::is_otlp_transport_allowed;
use crate::otlp_sender::start_otlp_transport;
use crate::otlp_spool_purge::PurgeOutcome;
use crate::otlp_spool_purge::PurgeReport;
use crate::otlp_spool_purge::PurgeSeams;
use crate::otlp_spool_purge::purge_report;
use crate::otlp_spool_purge::purge_spool;
use crate::types::TelemetryDiagnosticEvent;
use crate::types::TelemetryDiagnosticInput;
use crate::types::TelemetryDiagnostics;
use crate::types::TelemetryError;
use crate::types::TelemetrySpoolEntryKind;

/// The collector transport, or `None` whenever an opt-out applies or consent is not given.
pub fn create_otlp_telemetry_transport(
    input: CreateOtlpTelemetryTransportInput,
) -> Option<OtlpTelemetryTransport> {
    if !is_otlp_transport_allowed(&input) {
        purge_on_opt_out(&input);
        return None;
    }
    start_otlp_transport(input)
}

/// The opt-out purge with the diagnostics sink wired to its report; every entry it leaves behind
/// becomes one diagnostic, by kind or errno code, never by path.
pub fn purge_owned_spool(
    dir: &Path,
    root: &Path,
    diagnostics: Option<&TelemetryDiagnostics>,
    source: &str,
) -> io::Result<PurgeOutcome> {
    let on_skipped =
        |kind: TelemetrySpoolEntryKind| purge_report(diagnostics, source, Some(kind), None);
    let on_failed = |code: &str| purge_report(diagnostics, source, None, Some(code));
    let report = PurgeReport {
        on_skipped: &on_skipped,
        on_failed: &on_failed,
    };
    purge_spool(dir, root, &report, &PurgeSeams::default())
}

fn purge_on_opt_out(input: &CreateOtlpTelemetryTransportInput) {
    let source = input.source.clone().unwrap_or_else(|| "otlp".to_string());
    match purge_owned_spool(
        &input.spool_dir,
        &input.spool_root,
        input.diagnostics.as_ref(),
        &source,
    ) {
        Ok(PurgeOutcome::Refused) => {
            if let Some(diagnostics) = &input.diagnostics {
                diagnostics(&TelemetryDiagnosticInput {
                    event: TelemetryDiagnosticEvent::TelemetrySpoolPurgeRefused,
                    source,
                    error: None,
                    error_kind: None,
                    entry_kind: None,
                    error_code: None,
                    count: None,
                });
            }
        }
        Ok(_) => {}
        Err(error) => {
            if let Some(diagnostics) = &input.diagnostics {
                diagnostics(&TelemetryDiagnosticInput {
                    event: TelemetryDiagnosticEvent::TelemetrySpoolSweepFailed,
                    source,
                    error: Some(TelemetryError::new(error.to_string())),
                    error_kind: Some(crate::types::TelemetryDiagnosticErrorKind::Error),
                    entry_kind: None,
                    error_code: None,
                    count: None,
                });
            }
        }
    }
}
