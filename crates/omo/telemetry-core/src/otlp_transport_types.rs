//! Rust port of `packages/telemetry-core/src/otlp-transport-types.ts`.

use std::path::PathBuf;
use std::sync::Arc;

use crate::otel_time::InstrumentationScope;
use crate::otel_time::OtelClock;
use crate::otlp_config::OtlpWireProjection;
use crate::otlp_encode::ResourceAttributes;
use crate::otlp_http::OtlpFetch;
use crate::otlp_http::RandomFn;
use crate::otlp_sleep::SleepFn;
use crate::otlp_wal::SpanAllowlist;
use crate::types::TelemetryDiagnostics;
use crate::types::TelemetryEnv;

pub type Consent = Arc<dyn Fn() -> bool + Send + Sync>;
pub type NowMs = Arc<dyn Fn() -> u64 + Send + Sync>;
pub type IsPidAlive = Arc<dyn Fn(u32) -> bool + Send + Sync>;
pub type CreateEventId = Arc<dyn Fn() -> String + Send + Sync>;

#[derive(Clone)]
pub struct CreateOtlpTelemetryTransportInput {
    pub product_env_prefix: String,
    pub ingest_key: String,
    pub spool_dir: PathBuf,
    /// Required: the opt-out purge refuses a spool dir whose real path is not strictly below this one's.
    pub spool_root: PathBuf,
    pub resource: ResourceAttributes,
    pub scope: InstrumentationScope,
    pub env: Option<TelemetryEnv>,
    pub endpoint: Option<String>,
    /// Required: the schema artifact's per-event allowlist; there is no default, so a caller cannot
    /// ship unlisted properties.
    pub project: OtlpWireProjection,
    /// Required: the schema artifact's span allowlist, span name -> the attribute keys it may carry.
    pub spans: SpanAllowlist,
    pub consent: Option<Consent>,
    pub diagnostics: Option<TelemetryDiagnostics>,
    pub source: Option<String>,
    pub fetch: Option<OtlpFetch>,
    /// Per-request deadline, response body included (default 10 s).
    pub request_timeout_ms: Option<u64>,
    pub lease_renew_interval_ms: Option<u64>,
    pub sleep: Option<SleepFn>,
    pub random: Option<RandomFn>,
    pub now_ms: Option<NowMs>,
    pub clock: Option<OtelClock>,
    pub is_pid_alive: Option<IsPidAlive>,
    pub pid: Option<u32>,
    pub boot_id: Option<String>,
    pub create_event_id: Option<CreateEventId>,
    pub auto_start: Option<bool>,
    pub flush_at: Option<usize>,
    pub flush_interval_ms: Option<u64>,
    pub sweep_interval_ms: Option<u64>,
    pub shutdown_timeout_ms: Option<u64>,
    pub cap_bytes: Option<u64>,
    pub max_age_ms: Option<u64>,
}

/// `sampling: { rawTurnRate: 1 }` upstream: the collector transport always records raw turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sampling {
    pub raw_turn_rate: u8,
}

impl Default for Sampling {
    fn default() -> Self {
        Self { raw_turn_rate: 1 }
    }
}
