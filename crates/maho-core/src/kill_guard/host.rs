//! Port of `packages/coding-agent/src/core/kill-guard/host.ts` (senpi#5349).

use super::process_view::{KillGuardContextOptions, kill_guard_context, system_spawn_sync};
use super::protected::{minimal_protected, protected_set};
use super::request::default_platform;
use super::types::{KillGuardContext, KillGuardShell, KillGuardVantage};
use crate::settings_manager::SettingsManager;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillGuardSetting {
    On,
    Off,
}

/// `terminal.killGuard` from USER settings only: a project cannot switch the guard off for itself.
pub fn read_kill_guard_setting(cwd: &Path, agent_dir: Option<&Path>) -> KillGuardSetting {
    let resolved_agent = agent_dir
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(crate::config::get_agent_dir);
    let cwd_str = cwd.to_string_lossy();
    let home = crate::config::home_dir();
    let settings = SettingsManager::create(&cwd_str, &resolved_agent, &home, false);
    let is_off = settings
        .get_global()
        .get("terminal")
        .and_then(|terminal| terminal.get("killGuard"))
        .and_then(|value| value.as_str())
        == Some("off");
    if is_off {
        KillGuardSetting::Off
    } else {
        KillGuardSetting::On
    }
}

fn host_parent_pid() -> Option<i64> {
    #[cfg(unix)]
    {
        Some(i64::from(std::os::unix::process::parent_id()))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// A decision context for a signal the host itself will issue on the agent's behalf.
pub fn host_kill_guard_context(
    vantage: KillGuardVantage,
    shell: KillGuardShell,
) -> KillGuardContext {
    kill_guard_context(KillGuardContextOptions {
        platform: default_platform().to_owned(),
        agent_pid: i64::from(std::process::id()),
        agent_ppid: host_parent_pid(),
        vantage,
        shell,
        spawn_sync: system_spawn_sync(),
    })
}

/// The protected set P at this moment, exported to kernels as `SENPI_KILL_GUARD_PROTECTED` (their cheap prefilter).
pub fn kill_guard_protected_pids() -> Vec<i64> {
    let context = host_kill_guard_context(KillGuardVantage::Agent, KillGuardShell::Argv);
    let view = context.view();
    let protected_pids = match &view {
        None => minimal_protected(&context),
        Some(view) => protected_set(&context, &view.rows),
    };
    let mut pids: Vec<i64> = protected_pids.keys().copied().collect();
    pids.sort_unstable();
    pids
}
