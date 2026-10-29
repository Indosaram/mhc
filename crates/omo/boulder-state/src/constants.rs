//! On-disk locations used by the boulder state machine.

/// Directory (relative to a worktree root) that holds boulder state.
pub const BOULDER_DIR: &str = ".omo";
/// File name of the serialized boulder state document.
pub const BOULDER_FILE: &str = "boulder.json";
/// `BOULDER_DIR`/`BOULDER_FILE`, as the TypeScript package exposes it.
pub const BOULDER_STATE_PATH: &str = ".omo/boulder.json";
/// Sub-directory of `BOULDER_DIR` holding work notepads.
pub const NOTEPAD_DIR: &str = "notepads";
/// `BOULDER_DIR`/`NOTEPAD_DIR`, as the TypeScript package exposes it.
pub const NOTEPAD_BASE_PATH: &str = ".omo/notepads";
/// Plans directory written by the Prometheus planner.
pub const PROMETHEUS_PLANS_DIR: &str = ".omo/plans";
/// Legacy plans directory, still scanned for backwards compatibility.
pub(crate) const LEGACY_PROMETHEUS_PLANS_DIR: &str = ".sisyphus/plans";
