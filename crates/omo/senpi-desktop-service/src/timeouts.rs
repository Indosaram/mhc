//! Timeout constants matching upstream `@oh-my-opencode/senpi-desktop-service/src/service/timeouts.ts`.

use std::time::Duration;

/// Bounds spawn + `engine.hello` + `session.open` of a fresh engine child.
pub const START_TIMEOUT_MS: u64 = 10_000;
pub const START_TIMEOUT: Duration = Duration::from_millis(START_TIMEOUT_MS);

/// Bounds `session.close` plus the child's exit at stdin EOF before it is killed.
pub const CLOSE_TIMEOUT_MS: u64 = 1_500;
pub const CLOSE_TIMEOUT: Duration = Duration::from_millis(CLOSE_TIMEOUT_MS);

/// How long a timed-out or aborted request may take to answer `$/cancel` before the child is killed.
pub const GRACE_MS: u64 = 750;
pub const GRACE_TIMEOUT: Duration = Duration::from_millis(GRACE_MS);

/// Bounds a `capabilities` request.
pub const CAPABILITIES_TIMEOUT_MS: u64 = 10_000;
pub const CAPABILITIES_TIMEOUT: Duration = Duration::from_millis(CAPABILITIES_TIMEOUT_MS);

/// Cadence of `stopPath.heartbeat` while a session is open.
pub const HEARTBEAT_MS: u64 = 500;
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(HEARTBEAT_MS);

/// Why a request failed after the child was killed for ignoring `$/cancel`.
pub const RESTART_MESSAGE: &str = "desktop engine restarted; captures and ax refs were reset";

/// Why `open` failed when the engine never finished starting.
pub const START_TIMEOUT_MESSAGE: &str = "Timed out starting desktop engine";
