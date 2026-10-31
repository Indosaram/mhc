//! Port of `packages/omo-senpi/src/components/telemetry/otlp-runtime.ts`.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::json;
use telemetry_core::{
    CreateOtlpTelemetryTransportInput, OtlpTelemetryTransport, TelemetryCaptureMessage,
    TelemetryDeliveryError, TelemetryError, TelemetryOsProvider, TelemetryTransport, TurnTracing,
    create_otlp_telemetry_transport, otel_host_arch,
};

use crate::otlp::TransportInput;
use crate::otlp_events::is_otlp_event;
use crate::otlp_projection::{otlp_projection, otlp_spans};

/// The collector transport that drops every event outside the pinned OTLP set on capture.
struct EventFilteredOtlp(OtlpTelemetryTransport);

impl TelemetryTransport for EventFilteredOtlp {
    fn capture(&self, message: &TelemetryCaptureMessage) -> Result<(), TelemetryError> {
        if is_otlp_event(&message.event) {
            self.0.capture(message)
        } else {
            Ok(())
        }
    }
    fn capture_immediate<'a>(
        &'a self,
        message: &'a TelemetryCaptureMessage,
    ) -> Option<BoxFuture<'a, Result<(), TelemetryDeliveryError>>> {
        self.0.capture_immediate(message)
    }
    fn traces(&self) -> Option<&dyn TurnTracing> {
        TelemetryTransport::traces(&self.0)
    }
    fn flush(&self) -> Option<BoxFuture<'_, Result<(), TelemetryError>>> {
        TelemetryTransport::flush(&self.0)
    }
    fn shutdown(&self) -> BoxFuture<'_, Result<(), TelemetryError>> {
        TelemetryTransport::shutdown(&self.0)
    }
}

/// `os.version`: the leading dotted-number run of the release (two to four parts), else "0.0".
fn os_version(release: &str) -> String {
    let mut parts = 0;
    let mut end = 0;
    let mut cursor = 0;
    for segment in release.split('.') {
        let digits = segment.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || parts == 4 {
            break;
        }
        parts += 1;
        end = cursor + digits;
        if digits < segment.len() {
            break;
        }
        cursor += segment.len() + 1;
    }
    if parts >= 2 {
        release[..end].to_string()
    } else {
        "0.0".to_string()
    }
}

/// No key means no collector sender, spool or network activity; PostHog is independent.
pub fn create_senpi_otlp_transport(
    input: &TransportInput,
    ingest_key: &str,
    message: &TelemetryCaptureMessage,
) -> Option<Arc<dyn TelemetryTransport>> {
    if ingest_key.is_empty() || !is_otlp_event(&message.event) {
        return None;
    }
    let properties = &message.properties;
    let version = properties
        .get("package_version")
        .and_then(|value| value.as_str())
        .unwrap_or("0.0.0")
        .to_string();
    let os = telemetry_core::SystemTelemetryOsProvider;
    let platform = os.platform();
    let spool_root = crate::index::get_senpi_agent_dir(&input.env);
    let mut instance = [0_u8; 16];
    getrandom::fill(&mut instance).ok()?;
    let resource = serde_json::Map::from_iter([
        ("service.name".to_string(), json!("omo-senpi")),
        ("service.version".to_string(), json!(version)),
        ("service.instance.id".to_string(), json!(hex_of(&instance))),
        ("omo.install.hash".to_string(), json!(message.distinct_id)),
        ("omo.schema.version".to_string(), json!(4)),
        (
            "omo.surface".to_string(),
            json!(
                if properties.get("surface").and_then(|v| v.as_str()) == Some("desktop") {
                    "desktop"
                } else {
                    "cli"
                }
            ),
        ),
        (
            "omo.ci".to_string(),
            json!(properties.get("ci") == Some(&json!(true))),
        ),
        (
            "os.type".to_string(),
            json!(if platform == "win32" {
                "windows"
            } else {
                platform.as_str()
            }),
        ),
        ("os.version".to_string(), json!(os_version(&os.release()))),
        ("host.arch".to_string(), json!(otel_host_arch(&os.arch()))),
        ("telemetry.sdk.name".to_string(), json!("omo-otel-minimal")),
        ("telemetry.sdk.language".to_string(), json!("nodejs")),
        ("telemetry.sdk.version".to_string(), json!("1.0.0")),
    ]);
    let transport = create_otlp_telemetry_transport(CreateOtlpTelemetryTransportInput {
        product_env_prefix: "OMO_SENPI".into(),
        ingest_key: ingest_key.into(),
        spool_dir: spool_root.join("omo-senpi").join("otlp"),
        spool_root,
        resource,
        scope: telemetry_core::InstrumentationScope {
            name: "omo-senpi".into(),
            version,
        },
        env: Some(input.env.clone()),
        endpoint: None,
        project: otlp_projection(),
        spans: otlp_spans(),
        consent: Some(Arc::clone(&input.consent)),
        diagnostics: input.diagnostics.clone(),
        source: input.source.clone(),
        fetch: None,
        request_timeout_ms: None,
        lease_renew_interval_ms: None,
        sleep: None,
        random: None,
        now_ms: None,
        clock: None,
        is_pid_alive: None,
        pid: None,
        boot_id: None,
        create_event_id: None,
        auto_start: None,
        flush_at: None,
        flush_interval_ms: None,
        sweep_interval_ms: None,
        shutdown_timeout_ms: Some(500),
        cap_bytes: None,
        max_age_ms: None,
    })?;
    Some(Arc::new(EventFilteredOtlp(transport)))
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::os_version;

    #[test]
    fn os_version_keeps_the_leading_dotted_run() {
        assert_eq!(os_version("6.1.9-arch1-2"), "6.1.9");
        assert_eq!(os_version("10.0.22631"), "10.0.22631");
        assert_eq!(os_version("26.3"), "26.3");
        assert_eq!(os_version("7"), "0.0");
        assert_eq!(os_version(""), "0.0");
        assert_eq!(os_version("1.2.3.4.5"), "1.2.3.4");
    }
}
