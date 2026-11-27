//! Port of `restore-handlers.ts`:
//! Wraps the durability restore handlers with optional kill-guard hooks (`senpi#5349`).
//! Until Wave 2 merges `maho_core::kill_guard`, callers may inject `kill_guard_on` / `judge`
//! via `RestartableCommandDeps` (defaulting to `KillGuardVerdict::Allow`).

use crate::durable_command::{
    KillGuardVerdict, RestartableCommandDeps, RestoreEnvironment,
    restore_configured_command_in_with_deps,
};
use crate::manager::TerminalManager;
use crate::monitor_registry::CommandMonitor;
use crate::restore::RestoreOutcome;
use crate::runtime_session::{RuntimeError, TerminalRuntimeSession};
use crate::settings::ResolvedTerminalSettings;
use crate::terminal_manifest_model::ManifestMonitor;
use std::sync::Arc;

pub type KillGuardOnFn = Arc<dyn Fn() -> bool + Send + Sync>;
pub type KillGuardJudgeFn = Arc<dyn Fn(&str) -> KillGuardVerdict + Send + Sync>;

/// Build `RestartableCommandDeps` with optional kill-guard hooks; defaults to allowing restored commands
/// when no judge is injected.
pub fn restartable_command_deps(
    kill_guard_on: Option<KillGuardOnFn>,
    judge: Option<KillGuardJudgeFn>,
) -> RestartableCommandDeps {
    RestartableCommandDeps {
        kill_guard_on,
        judge,
    }
}

/// Restore a restartable-command monitor through the kill-guard seam.
pub fn restore_command_monitor(
    monitor: &ManifestMonitor,
    manager: &mut TerminalManager,
    shell: Option<&str>,
    settings: &ResolvedTerminalSettings,
    context: &RestoreEnvironment,
    deps: &RestartableCommandDeps,
    register: impl FnMut(&str, &TerminalRuntimeSession, CommandMonitor) -> Result<(), RuntimeError>,
) -> (RestoreOutcome, Option<String>) {
    restore_configured_command_in_with_deps(
        monitor, manager, shell, settings, context, deps, register,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::restore::parse_terminal_manifest;
    use crate::settings::TERMINAL_SETTINGS_DEFAULTS;
    use serde_json::json;

    fn sample_monitor() -> ManifestMonitor {
        let tmp = std::env::temp_dir();
        let tmp_str = tmp.to_str().expect("temp dir utf8");
        parse_terminal_manifest(
            &json!({
                "monitors": [{
                    "monitorId": "mon_guard_1",
                    "sessionId": "s",
                    "description": "watch",
                    "runtimeKind": "command",
                    "durabilityClass": "restartable-command",
                    "command": "echo hi",
                    "cwd": tmp_str,
                    "createdAt": 1,
                    "expiresAt": null,
                    "persistent": true,
                    "suspended": false,
                    "lastCheckpoint": null,
                    "deliveryPaused": false,
                    "fireWindow": { "startMs": 1, "count": 0 }
                }],
                "backgroundSessions": [],
                "updatedAt": 1
            }),
            "s",
        )
        .expect("manifest")
        .monitors
        .remove(0)
    }

    #[test]
    fn kill_guard_refuses_when_on_and_allows_when_off() {
        let monitor = sample_monitor();
        let mut manager = TerminalManager::default();
        let deps_refuse = restartable_command_deps(
            Some(Arc::new(|| true)),
            Some(Arc::new(|cmd| KillGuardVerdict::Refuse {
                code: "kill_guard".to_owned(),
                message: format!("blocked {cmd}"),
            })),
        );
        let (outcome, reason) = restore_command_monitor(
            &monitor,
            &mut manager,
            None,
            &TERMINAL_SETTINGS_DEFAULTS,
            &RestoreEnvironment::default(),
            &deps_refuse,
            |_, _, _| Ok(()),
        );
        assert_eq!(outcome, RestoreOutcome::Lost);
        assert_eq!(reason.as_deref(), Some("blocked echo hi"));

        let deps_off = restartable_command_deps(
            Some(Arc::new(|| false)),
            Some(Arc::new(|_| KillGuardVerdict::Refuse {
                code: "kill_guard".to_owned(),
                message: "should not fire".to_owned(),
            })),
        );
        let (outcome_off, reason_off) = restore_command_monitor(
            &monitor,
            &mut manager,
            None,
            &TERMINAL_SETTINGS_DEFAULTS,
            &RestoreEnvironment::default(),
            &deps_off,
            |_, _, _| Ok(()),
        );
        assert_eq!(outcome_off, RestoreOutcome::Restored);
        assert_eq!(reason_off, None);
    }
}
