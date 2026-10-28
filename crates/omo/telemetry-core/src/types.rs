use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use futures::future::BoxFuture;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

/// Environment snapshot read instead of the process environment when supplied.
pub type TelemetryEnv = HashMap<String, String>;

/// Property bag attached to a capture message; insertion order is preserved on the wire.
pub type TelemetryCaptureProperties = Map<String, Value>;

/// One analytics event handed to a [`TelemetryTransport`].
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryCaptureMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub distinct_id: String,
    pub event: String,
    pub properties: TelemetryCaptureProperties,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryDiagnosticEvent {
    TelemetryBufferOverflow,
    TelemetryActivityStateReadFailed,
    TelemetryActivityStateWriteFailed,
    TelemetryCaptureFailed,
    TelemetryConsentFailed,
    TelemetryCpuInfoUnavailable,
    TelemetryDropFlushFailed,
    TelemetryEventPropertyDropped,
    TelemetryEventPropertyRejected,
    TelemetryEventRejected,
    TelemetryEventsDroppedByConsent,
    TelemetryLegacyRollupsRemoved,
    TelemetryOtlpPartialSuccess,
    TelemetryOtlpSendFailed,
    TelemetryPosthogImportFailed,
    TelemetryPosthogInitFailed,
    TelemetryShutdownFailed,
    TelemetrySpoolPurgeFailed,
    TelemetrySpoolPurgeRefused,
    TelemetrySpoolPurgeSkipped,
    TelemetrySpoolSweepFailed,
    TelemetrySpoolWriteFailed,
}

impl TelemetryDiagnosticEvent {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TelemetryBufferOverflow => "telemetry_buffer_overflow",
            Self::TelemetryActivityStateReadFailed => "telemetry_activity_state_read_failed",
            Self::TelemetryActivityStateWriteFailed => "telemetry_activity_state_write_failed",
            Self::TelemetryCaptureFailed => "telemetry_capture_failed",
            Self::TelemetryConsentFailed => "telemetry_consent_failed",
            Self::TelemetryDropFlushFailed => "telemetry_drop_flush_failed",
            Self::TelemetryCpuInfoUnavailable => "telemetry_cpu_info_unavailable",
            Self::TelemetryEventPropertyDropped => "telemetry_event_property_dropped",
            Self::TelemetryEventPropertyRejected => "telemetry_event_property_rejected",
            Self::TelemetryEventRejected => "telemetry_event_rejected",
            Self::TelemetryEventsDroppedByConsent => "telemetry_events_dropped_by_consent",
            Self::TelemetryLegacyRollupsRemoved => "telemetry_legacy_rollups_removed",
            Self::TelemetryOtlpPartialSuccess => "telemetry_otlp_partial_success",
            Self::TelemetryOtlpSendFailed => "telemetry_otlp_send_failed",
            Self::TelemetryPosthogImportFailed => "telemetry_posthog_import_failed",
            Self::TelemetryPosthogInitFailed => "telemetry_posthog_init_failed",
            Self::TelemetryShutdownFailed => "telemetry_shutdown_failed",
            Self::TelemetrySpoolPurgeFailed => "telemetry_spool_purge_failed",
            Self::TelemetrySpoolPurgeRefused => "telemetry_spool_purge_refused",
            Self::TelemetrySpoolPurgeSkipped => "telemetry_spool_purge_skipped",
            Self::TelemetrySpoolSweepFailed => "telemetry_spool_sweep_failed",
            Self::TelemetrySpoolWriteFailed => "telemetry_spool_write_failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryDiagnosticErrorKind {
    Error,
    NonError,
}

impl TelemetryDiagnosticErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::NonError => "non_error",
        }
    }
}

/// An error surfaced to diagnostics: the `name`/`message` pair of the TS `Error` it replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryError {
    pub name: String,
    pub message: String,
}

impl TelemetryError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            name: "Error".to_string(),
            message: message.into(),
        }
    }

    pub fn named(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for TelemetryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.name, self.message)
    }
}

impl std::error::Error for TelemetryError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetrySpoolEntryKind {
    Directory,
    Symlink,
    ForeignOwner,
    Other,
}

impl TelemetrySpoolEntryKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Directory => "directory",
            Self::Symlink => "symlink",
            Self::ForeignOwner => "foreign_owner",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryDiagnosticInput {
    pub event: TelemetryDiagnosticEvent,
    pub source: String,
    pub error: Option<TelemetryError>,
    pub error_kind: Option<TelemetryDiagnosticErrorKind>,
    pub entry_kind: Option<TelemetrySpoolEntryKind>,
    pub error_code: Option<String>,
    pub count: Option<u64>,
}

impl TelemetryDiagnosticInput {
    pub fn failure(event: TelemetryDiagnosticEvent, source: &str, error: TelemetryError) -> Self {
        Self {
            event,
            source: source.to_string(),
            error: Some(error),
            error_kind: Some(TelemetryDiagnosticErrorKind::Error),
            entry_kind: None,
            error_code: None,
            count: None,
        }
    }
}

/// Shared diagnostics sink held by long-lived clients.
pub type TelemetryDiagnostics = Arc<dyn Fn(&TelemetryDiagnosticInput) + Send + Sync>;

/// Partial transport options a product may layer over the defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TelemetryTransportOverrides {
    pub host: Option<String>,
    pub disable_geoip: Option<bool>,
    pub enable_exception_autocapture: Option<bool>,
    pub enable_local_evaluation: Option<bool>,
    pub strict_local_evaluation: Option<bool>,
    pub disable_remote_config: Option<bool>,
    pub flush_at: Option<u32>,
    pub flush_interval: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TelemetryProductConfig {
    pub cache_dir_name: String,
    pub default_api_key: String,
    pub default_host: String,
    pub event_name: String,
    pub machine_id_prefix: String,
    pub package_name: String,
    pub package_version: String,
    pub platform: String,
    pub product_env_prefix: String,
    pub product_name: String,
    pub additional_properties: Option<TelemetryCaptureProperties>,
    pub disable_geoip: Option<bool>,
    pub transport_options: Option<TelemetryTransportOverrides>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryCpuInfo {
    pub model: String,
}

/// Host facts reported with daily-active events; mirrors the subset of `node:os` the TS package reads.
pub trait TelemetryOsProvider: Send + Sync {
    fn arch(&self) -> String;
    fn cpus(&self) -> Result<Vec<TelemetryCpuInfo>, TelemetryError>;
    fn hostname(&self) -> String;
    fn platform(&self) -> String;
    fn release(&self) -> String;
    fn totalmem(&self) -> u64;
    fn os_type(&self) -> String;
}

/// Resolved options passed to a transport factory. Field order is the wire order of the TS defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryTransportOptions {
    pub enable_exception_autocapture: bool,
    pub enable_local_evaluation: bool,
    pub strict_local_evaluation: bool,
    pub disable_remote_config: bool,
    pub flush_at: u32,
    pub flush_interval: u64,
    pub host: String,
    pub disable_geoip: bool,
}

impl TelemetryTransportOptions {
    pub(crate) fn apply(&mut self, overrides: &TelemetryTransportOverrides) {
        if let Some(host) = &overrides.host {
            self.host.clone_from(host);
        }
        if let Some(value) = overrides.disable_geoip {
            self.disable_geoip = value;
        }
        if let Some(value) = overrides.enable_exception_autocapture {
            self.enable_exception_autocapture = value;
        }
        if let Some(value) = overrides.enable_local_evaluation {
            self.enable_local_evaluation = value;
        }
        if let Some(value) = overrides.strict_local_evaluation {
            self.strict_local_evaluation = value;
        }
        if let Some(value) = overrides.disable_remote_config {
            self.disable_remote_config = value;
        }
        if let Some(value) = overrides.flush_at {
            self.flush_at = value;
        }
        if let Some(value) = overrides.flush_interval {
            self.flush_interval = value;
        }
    }
}

/// Delivery backend for capture messages (PostHog by default, a recorder in tests).
///
/// `capture` is synchronous and may only enqueue; `flush` returns `None` when the transport has
/// no flush step, matching the optional `flush` of the TS contract.
pub trait TelemetryTransport: Send + Sync {
    fn capture(&self, message: &TelemetryCaptureMessage) -> Result<(), TelemetryError>;
    fn capture_immediate<'a>(
        &'a self,
        _message: &'a TelemetryCaptureMessage,
    ) -> Option<BoxFuture<'a, Result<(), crate::telemetry_delivery_error::TelemetryDeliveryError>>>
    {
        None
    }
    fn traces(&self) -> Option<&dyn TurnTracing> {
        None
    }
    /// Durable send: resolves only after acknowledgment, never merely after enqueueing.
    fn capture_durable(
        &self,
        message: TelemetryCaptureMessage,
    ) -> BoxFuture<'_, Result<(), TelemetryError>> {
        Box::pin(async move { self.capture(&message) })
    }
    fn flush(&self) -> Option<BoxFuture<'_, Result<(), TelemetryError>>>;
    fn shutdown(&self) -> BoxFuture<'_, Result<(), TelemetryError>>;
}

pub type TelemetryTransportFactory = Arc<
    dyn Fn(&str, &TelemetryTransportOptions) -> Result<Box<dyn TelemetryTransport>, TelemetryError>
        + Send
        + Sync,
>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackActiveInput {
    pub day_utc: String,
    pub distinct_id: String,
    pub reason: String,
}

/// Primitive-only turn attributes, corresponding to the OpenTelemetry public API.
pub type TurnSpanAttributes = std::collections::BTreeMap<String, TurnSpanAttribute>;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum TurnSpanAttribute {
    String(String),
    Number(f64),
    Boolean(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnSpanStatus {
    Ok,
    Error,
}

#[derive(Debug, Clone, Default)]
pub struct TurnSpanStart {
    pub attributes: TurnSpanAttributes,
    pub parent_traceparent: Option<String>,
}

pub trait TurnTracing: Send + Sync {
    fn start_turn(&self, start: TurnSpanStart) -> Option<crate::otel_turn_span::TurnSpanHandle>;
}
