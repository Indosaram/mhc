//! Port of `packages/machine-ops/src/limits.ts`.

use crate::types::Limits;

const MIB: usize = 1024 * 1024;

/// Machine-path defaults: herdr exec caps, bunshin read cap and shell budget, no PTY cap.
pub const DEFAULT_LIMITS: Limits = Limits {
    read_max_bytes: Some(10 * MIB),
    exec_stdout_max_bytes: 8 * MIB,
    exec_stderr_max_bytes: MIB,
    exec_timeout_default_ms: Some(3_600_000),
    pty_scrollback_bytes: 2 * MIB,
    retained_output_bytes: MIB,
    max_pty_sessions: None,
};
