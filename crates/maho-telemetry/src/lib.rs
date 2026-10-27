//! Opt-out-respecting telemetry for mhc (MIT profile).
//!
//! - [`settings`]: who may record what — `MAHO_TELEMETRY`, `DO_NOT_TRACK`, `OTEL_SDK_DISABLED`,
//!   `MAHO_USAGE_STATS` and the `telemetry.*` config keys, plus the `OTEL_*` per-signal rules.
//! - [`record`] and [`stats`]: the local usage ledger (`<agent dir>/stats/usage.jsonl`), the
//!   session-log scan and the `mhc usage` report.
//! - [`otlp`] and [`proto`]: OTLP/HTTP protobuf export of GenAI traces, run metrics and a run log
//!   to a user-configured collector. Nothing leaves the machine without an `OTEL_*` endpoint.
//! - [`extension`]: `<builtin:telemetry>`, which feeds both from agent events.
//!
//! Ported from MIT can1357/oh-my-pi (telemetry export, stats) where an original exists; the
//! opt-out variables, the ledger and `mhc usage` follow docs/cleanroom/specs/telemetry.md. See
//! PROVENANCE.md.
pub mod cli;
pub mod extension;
pub mod otlp;
pub mod proto;
pub mod record;
pub mod settings;
pub mod stats;

pub use extension::TelemetryExtension;
pub use settings::{ConfigSwitches, Settings, resolve};
