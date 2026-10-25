//! Rust port of `packages/telemetry-core/src/otlp-config.ts`.

use std::sync::Arc;

use serde_json::Map;
use serde_json::Value;

use crate::env::ShouldDisableTelemetryInput;
use crate::env::read_env;
use crate::env::should_disable_telemetry;
use crate::types::TelemetryEnv;

pub const DEFAULT_OTLP_ENDPOINT: &str = "https://t.omo.dev";

/// `OMO_TELEMETRY_ENDPOINT` overrides the configured endpoint. The standard `OTEL_EXPORTER_OTLP_*`
/// variables are deliberately never read: a machine-wide OpenTelemetry setup must not redirect
/// product telemetry.
pub fn resolve_otlp_endpoint(env: Option<&TelemetryEnv>, configured: Option<&str>) -> String {
    let override_value = read_env(env, "OMO_TELEMETRY_ENDPOINT")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let base = override_value
        .or_else(|| configured.map(str::to_string))
        .unwrap_or_else(|| DEFAULT_OTLP_ENDPOINT.to_string());
    base.trim_end_matches('/').to_string()
}

pub fn is_otlp_telemetry_enabled(env: Option<&TelemetryEnv>, product_env_prefix: &str) -> bool {
    if read_env(env, "OTEL_SDK_DISABLED")
        .map(|value| value.trim().to_lowercase())
        .as_deref()
        == Some("true")
    {
        return false;
    }
    !should_disable_telemetry(&ShouldDisableTelemetryInput {
        env,
        global_env_prefix: None,
        product_env_prefix,
    })
}

pub type WireAttributes = Map<String, Value>;

/// The schema artifact's per-event allowlist, mapping each allowlisted property to its wire key.
pub type OtlpWireProjection =
    Arc<dyn Fn(&str, &Map<String, Value>) -> WireAttributes + Send + Sync>;

const RESOURCE_OR_RELAY_KEYS: [&str; 12] = [
    "$os",
    "$os_version",
    "$process_person_profile",
    "$session_id",
    "ci",
    "event_id",
    "install_hash",
    "package_version",
    "platform",
    "product_name",
    "schema_version",
    "surface",
];

/// Resource-level and relay-only keys stay off the record and `$` keys never travel on the wire.
pub fn is_wire_attribute_key(key: &str) -> bool {
    !RESOURCE_OR_RELAY_KEYS.contains(&key) && !key.starts_with('$')
}
