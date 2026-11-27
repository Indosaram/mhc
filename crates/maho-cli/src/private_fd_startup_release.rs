//! Port of `packages/coding-agent/src/private-fd-startup-release.ts` (senpi#4602, #6769).

use std::collections::BTreeMap;

use super::private_fd_startup_fillers::{FillerFdOps, release_inherited_startup_fillers};

/// Releases a coordinated start's inherited filler pipes while the `#4749` private descriptor table
/// is still in the environment. Idempotent.
pub fn release_startup_fillers_in_env(
    env: &mut BTreeMap<String, String>,
    ops: &mut dyn FillerFdOps,
) {
    release_inherited_startup_fillers(env, ops);
}
