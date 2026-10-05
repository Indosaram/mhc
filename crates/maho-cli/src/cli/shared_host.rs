//! Port of senpi `core/shared-host-policy.ts` and the shared-host mount in
//! `main.ts` (`shouldJoinSharedHost` -> `createInteractiveHostRuntime` -> `selectedRuntime`).
//!
//! The client half of `interactive-host-runtime.ts` is owned by todo 35
//! (`maho_interactive::interactive_host_runtime`); this module owns the CLI decision, the socket
//! resolution and the warning surface senpi's `main.ts` applies around it.

use std::path::PathBuf;

use maho_core::project_trust::AppMode;
use maho_interactive::interactive_host_runtime::{
    create_interactive_host_runtime, HostSessionBinding, InteractiveHostRuntimeOptions,
    InteractiveHostRuntimeOutcome, InteractiveHostWarning,
};

/// senpi `envValue("ENABLE_SHARED_HOST")` brand-prefixed suffix.
pub const SHARED_HOST_ENABLE_ENV_SUFFIX: &str = "ENABLE_SHARED_HOST";

/// senpi `main.ts` `isTruthyEnvFlag`.
pub fn is_truthy_env_flag(value: Option<&str>) -> bool {
    let Some(value) = value else { return false };
    if value.is_empty() { return false; }
    value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
}

/// senpi `shouldJoinSharedHost`: shared host is off by default; interactive sessions opt in.
pub fn should_join_shared_host(app_mode: AppMode, enable_env: bool, setting_enabled: bool) -> bool {
    if app_mode != AppMode::Interactive { return false; }
    enable_env || setting_enabled
}

/// The flat native key for senpi's nested `experimental.sharedHost` setting.
pub const EXPERIMENTAL_SHARED_HOST_SETTING: &str = "experimentalSharedHost";

pub fn shared_host_setting_enabled(settings: &maho_core::settings_manager::SettingsManager) -> bool {
    settings.get_bool(EXPERIMENTAL_SHARED_HOST_SETTING).unwrap_or(false)
}

/// senpi `envValue("RPC_SOCKET") ?? resolve(agentDir, "rpc", "rpc.sock")`.
pub fn shared_host_socket(agent_dir: &str) -> String {
    let env = maho_core::config::current_env();
    maho_core::brand::env_value("RPC_SOCKET", &env)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| PathBuf::from(agent_dir).join("rpc").join("rpc.sock").to_string_lossy().into_owned())
}

/// senpi `main.ts` shared-host mount, minus the TUI handoff the interactive owner performs.
pub struct SharedHostMount {
    pub socket: String,
    pub outcome: InteractiveHostRuntimeOutcome,
}

impl SharedHostMount {
    pub fn warning(&self) -> Option<&InteractiveHostWarning> {
        match &self.outcome {
            InteractiveHostRuntimeOutcome::Remote(_) => None,
            InteractiveHostRuntimeOutcome::Fallback(warning) => Some(warning),
        }
    }

    /// senpi `console.error(chalk.yellow(warning.message))`.
    pub fn warning_message(&self) -> Option<&str> {
        self.warning().map(|warning| warning.message.as_str())
    }
}

pub async fn join_shared_host(
    binding: HostSessionBinding,
    agent_dir: &str,
    on_warning: Option<maho_interactive::interactive_host_runtime::InteractiveHostWarningHandler>,
) -> SharedHostMount {
    let socket = shared_host_socket(agent_dir);
    let options = InteractiveHostRuntimeOptions {
        socket: socket.clone(),
        agent_dir: Some(PathBuf::from(agent_dir)),
        ensure_host: None,
        on_warning,
    };
    let outcome = create_interactive_host_runtime(binding, options).await;
    SharedHostMount { socket, outcome }
}
