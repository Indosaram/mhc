//! This build as a host client: what it declares to `decide_host_action`, and when a host is compatible with it
//! (senpi `modes/rpc/host-ensure-client.ts`).

use crate::host_capabilities::{ToolRuntimeKind, tool_runtime_capability};
use crate::host_decision::{
    HOST_PROTOCOL_VERSION, HostDecision, HostDecisionClient, HostDecisionPolicy, HostProtocolInfo,
    REQUIRED_HOST_CAPABILITIES, decide_host_action,
};
use crate::host_ensure::{EnsureHostOptions, HostUpgradePolicy};
use crate::protocol_identity::host_launch_profile;

/// The argv the supervisor gives its host child; the launch profile has to describe THAT host (senpi `hostChildArgv`,
/// defined once in `host_ensure.rs`).
pub use crate::host_ensure::host_child_argv;

/// The platform name a host decision compares (`process.platform`).
fn platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else {
        std::env::consts::OS
    }
}

/// This build as a client: which protocol it speaks, what it needs from a host, and which build it is.
pub fn ensure_client(options: &EnsureHostOptions, started_by_us: bool) -> HostDecisionClient {
    let mut required_capabilities: Vec<String> = REQUIRED_HOST_CAPABILITIES
        .iter()
        .map(|capability| (*capability).to_owned())
        .collect();
    required_capabilities.extend(requested_tool_runtime(&options.host_args));
    HostDecisionClient {
        protocol_version: HOST_PROTOCOL_VERSION,
        required_capabilities,
        identity: maho_core::engine_build_identity::engine_build_identity().clone(),
        // An ensure that may upgrade has to say what it would launch: the superset rule refuses to
        // hand off to a generation that would drop what the running host loads. An ensure that may
        // not upgrade declares nothing, and therefore can never be the newer candidate.
        launch_profile: if options.upgrade == HostUpgradePolicy::IfEngineDiffers {
            let cwd = std::env::current_dir()
                .map(|cwd| cwd.to_string_lossy().into_owned())
                .unwrap_or_default();
            host_launch_profile(&host_child_argv(&options.host_args), &cwd).ok()
        } else {
            None
        },
        started_by_us,
        platform: platform().to_owned(),
    }
}

/// An explicit `--tool-runtime` in `host_args` is a requirement (senpi#3566): a host advertising another runtime - or none,
/// because it predates the flag - is not reused for this client. Without the flag the client requires nothing new.
pub fn requested_tool_runtime(host_args: &[String]) -> Vec<String> {
    for (index, arg) in host_args.iter().enumerate() {
        let value = if arg == "--tool-runtime" {
            host_args.get(index + 1).map(String::as_str)
        } else {
            arg.strip_prefix("--tool-runtime=")
        };
        if let Some(kind) = value.and_then(ToolRuntimeKind::parse) {
            return vec![tool_runtime_capability(kind)];
        }
    }
    Vec::new()
}

/// Compatibility, for the attach decision and the readiness gate alike: a host is compatible exactly
/// when a client that is forbidden to upgrade would attach to it. Never a version-string comparison (I2).
pub fn is_compatible(protocol: Option<&HostProtocolInfo>) -> bool {
    let options = EnsureHostOptions {
        socket: String::new(),
        ..Default::default()
    };
    matches!(
        decide_host_action(
            &ensure_client(&options, false),
            protocol,
            HostDecisionPolicy::Never
        ),
        HostDecision::Reuse { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn options(host_args: &[&str]) -> EnsureHostOptions {
        EnsureHostOptions {
            socket: "s".to_owned(),
            host_args: strings(host_args),
            ..Default::default()
        }
    }

    #[test]
    fn reads_tool_runtime_from_host_args_in_both_spellings_ignoring_unknown_values() {
        assert_eq!(
            requested_tool_runtime(&strings(&["--tool-runtime", "child"])),
            vec!["tool_runtime:child"]
        );
        assert_eq!(
            requested_tool_runtime(&strings(&["--listen", "x", "--tool-runtime=in-process"])),
            vec!["tool_runtime:in-process"]
        );
        assert!(requested_tool_runtime(&strings(&["--tool-runtime", "bogus"])).is_empty());
        assert!(requested_tool_runtime(&strings(&["--tool-runtime"])).is_empty());
        assert!(requested_tool_runtime(&[]).is_empty());
    }

    #[test]
    fn requires_the_requested_runtime_and_nothing_new_without_the_flag() {
        let mut expected: Vec<String> = REQUIRED_HOST_CAPABILITIES
            .iter()
            .map(|value| (*value).to_owned())
            .collect();
        assert_eq!(
            ensure_client(&options(&[]), false).required_capabilities,
            expected
        );
        expected.push("tool_runtime:child".to_owned());
        assert_eq!(
            ensure_client(&options(&["--tool-runtime", "child"]), false).required_capabilities,
            expected
        );
    }

    #[test]
    fn host_child_argv_leads_with_the_multi_session_rpc_flags() {
        assert_eq!(
            host_child_argv(&strings(&["--tool-runtime", "child"])),
            strings(&[
                "--mode",
                "rpc",
                "--multi-session",
                "--tool-runtime",
                "child"
            ])
        );
    }
}
