//! Polling and grace-period constants (milliseconds).

/// Polling interval for background session status checks.
pub const POLL_INTERVAL_BACKGROUND_MS: u64 = 2000;

/// Long-running subagent work can legitimately stay open for a while (60 minutes).
pub const SESSION_TIMEOUT_MS: u64 = 60 * 60 * 1000;

/// Status queries can transiently miss live sessions under load (30 seconds).
pub const SESSION_MISSING_GRACE_MS: u64 = 30 * 1000;

/// Session readiness polling interval.
pub const SESSION_READY_POLL_INTERVAL_MS: u64 = 500;

/// Session readiness maximum wait (10 seconds).
pub const SESSION_READY_TIMEOUT_MS: u64 = 10_000;

/// Grace period after spawn during which panes auto-activate without focus (5 seconds).
pub const AUTO_ACTIVATE_GRACE_MS: u64 = 5_000;
