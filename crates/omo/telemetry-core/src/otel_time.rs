//! Rust port of `packages/telemetry-core/src/otel-time.ts`.

use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use chrono::DateTime;
use chrono::Utc;

pub const OTEL_SDK_NAME: &str = "omo-otel-minimal";
pub const OTEL_SDK_VERSION: &str = "1.0.0";
pub const OTEL_SDK_LANGUAGE: &str = "nodejs";
pub const MAX_ATTRIBUTES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstrumentationScope {
    pub name: String,
    pub version: String,
}

/// Unix time in nanoseconds; the OTLP `fixed64` unit (`bigint` upstream).
pub type UnixNanos = u64;

pub type OtelClock = Arc<dyn Fn() -> UnixNanos + Send + Sync>;

/// The injectable `TimeInput` of `@opentelemetry/api`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TimeInput {
    Date(DateTime<Utc>),
    /// Unix milliseconds, as a JS `number`.
    Number(f64),
    /// `[seconds, nanoseconds]`.
    HrTime(u64, u32),
}

/// `performance.timeOrigin + performance.now()`, in nanoseconds.
pub fn system_clock() -> OtelClock {
    Arc::new(|| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        now.as_nanos() as UnixNanos
    })
}

pub fn to_unix_nanos(input: Option<TimeInput>, clock: &OtelClock) -> UnixNanos {
    let Some(input) = input else {
        return clock();
    };
    match input {
        TimeInput::Date(date) => {
            let millis = date.timestamp_millis();
            if millis < 0 { 0 } else { millis as UnixNanos * 1_000_000 }
        }
        TimeInput::Number(millis) => {
            let rounded = millis.round();
            if rounded <= 0.0 {
                0
            } else {
                (rounded as UnixNanos) * 1_000
            }
        }
        TimeInput::HrTime(seconds, nanos) => seconds
            .saturating_mul(1_000_000_000)
            .saturating_add(u64::from(nanos)),
    }
}
