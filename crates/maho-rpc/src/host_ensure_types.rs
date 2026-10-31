//! The options and result of `ensure_host`, kept apart so the ensure's helpers can name them without a cycle
//! (senpi `rpc/host-ensure-types.ts`).
//!
//! senpi's `presenterBootstrap` / `mcpPresenterEnforcement` ride the request's presenter scope here
//! (`mcp_presenter_bootstrap::with_presenter_launch`) instead of the options, because the enrollment channel is
//! an owned stream that the one spawn it enrolls consumes; `_test` hooks are not ported.
use std::collections::HashMap;

/// What an ensure may do to a host that is already running.
///
/// `Never` (the default) attaches or starts, and touches nothing that is already serving the socket.
/// `IfEngineDiffers` additionally allows a GENERATION HANDOFF when `decide_host_action` finds this build strictly
/// newer and its extension set a superset of the running host's - the running host then drains instead of dying,
/// so no session is ever ended by an upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HostUpgradePolicy {
    #[default]
    Never,
    IfEngineDiffers,
}

/// One ensure request: the endpoint, what to launch, and the lifecycle policy to record.
#[derive(Debug, Clone)]
pub struct EnsureHostOptions {
    pub socket: String,
    pub agent_dir: Option<std::path::PathBuf>,
    /// Host lifecycle policy recorded in settings.json (env overrides win at runtime).
    pub policy: Option<crate::host_launch_spec::HostLifecyclePolicyInput>,
    /// Extra CLI arguments forwarded through the supervisor to the host process.
    pub host_args: Vec<String>,
    /// Extensions the spawned host loads; they form its launch profile.
    pub extensions: Vec<String>,
    /// Environment for the spawned host; a `None` value removes an inherited variable.
    pub env: HashMap<String, Option<String>>,
    /// Whether a newer build may take the socket over from the running host. Defaults to `Never`.
    pub upgrade: HostUpgradePolicy,
    pub readiness_ms: u64,
    /// Credential lookup only (`--rpc-key-root`); endpoint keys come from here, the profile is unchanged.
    pub key_root: Option<std::path::PathBuf>,
}
impl Default for EnsureHostOptions {
    fn default() -> Self {
        Self {
            socket: String::new(),
            agent_dir: None,
            policy: None,
            host_args: vec![],
            extensions: vec![],
            env: HashMap::new(),
            upgrade: HostUpgradePolicy::Never,
            readiness_ms: 10_000,
            key_root: None,
        }
    }
}

/// The host a client ended up attached to, and whether this call started it (senpi `EnsuredHost`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnsuredHostInfo {
    pub pid: u32,
    pub socket: String,
    pub reused: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_default_to_never_upgrade_and_a_bounded_readiness() {
        let options = EnsureHostOptions::default();
        assert_eq!(options.upgrade, HostUpgradePolicy::Never);
        assert_eq!(HostUpgradePolicy::default(), HostUpgradePolicy::Never);
        assert_eq!(options.readiness_ms, 10_000);
        assert!(
            options.agent_dir.is_none() && options.policy.is_none() && options.key_root.is_none()
        );
        assert!(options.host_args.is_empty() && options.env.is_empty());
    }
}
