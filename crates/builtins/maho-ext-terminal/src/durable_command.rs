use crate::{
    manager::TerminalManager,
    monitor_registry::{CommandMonitor, MonitorFireWindow, MonitorSnapshotEntry},
    restore::RestoreOutcome,
    terminal_manifest_model::{ManifestMonitor, MonitorRuntimeKind},
};
use std::path::Path;

pub fn restore_command(
    monitor: &ManifestMonitor,
    manager: &mut TerminalManager,
    mut register: impl FnMut(
        &str,
        &crate::runtime_session::TerminalRuntimeSession,
        CommandMonitor,
    ) -> Result<(), crate::runtime_session::RuntimeError>,
) -> RestoreOutcome {
    restore_configured_command(
        monitor,
        manager,
        None,
        &crate::settings::TERMINAL_SETTINGS_DEFAULTS,
        &mut register,
    )
}
pub fn restore_configured_command(
    monitor: &ManifestMonitor,
    manager: &mut TerminalManager,
    shell: Option<&str>,
    settings: &crate::settings::ResolvedTerminalSettings,
    mut register: impl FnMut(
        &str,
        &crate::runtime_session::TerminalRuntimeSession,
        CommandMonitor,
    ) -> Result<(), crate::runtime_session::RuntimeError>,
) -> RestoreOutcome {
    if monitor.runtime_kind != MonitorRuntimeKind::Command || !monitor.persistent {
        return RestoreOutcome::Lost;
    }
    let Some(command) = monitor
        .command
        .as_deref()
        .filter(|command| !command.is_empty())
    else {
        return RestoreOutcome::Lost;
    };
    let Some(cwd) = monitor
        .cwd
        .as_deref()
        .filter(|cwd| Path::new(cwd).is_absolute() && Path::new(cwd).is_dir())
    else {
        return RestoreOutcome::Lost;
    };
    let Ok((options, stdin)) =
        crate::tools::spawn::command_options(command, Some(Path::new(cwd)), shell)
    else {
        return RestoreOutcome::Lost;
    };
    let options = options.size(settings.default_cols as u16, settings.default_rows as u16);
    let Ok(id) = manager.create(command, options) else {
        return RestoreOutcome::Lost;
    };
    if let Some(stdin) = &stdin
        && manager
            .get(&id)
            .is_none_or(|runtime| runtime.write(stdin.as_bytes()).is_err())
    {
        return RestoreOutcome::Lost;
    }
    let mut record = CommandMonitor::new(
        MonitorSnapshotEntry {
            id: id.clone(),
            monitor_id: Some(monitor.monitor_id.clone()),
            description: monitor.description.clone(),
            command: monitor.command.clone(),
            filter: monitor.filter.clone(),
            persistent: Some(true),
            deadline_ms: None,
            expires_at: monitor.expires_at,
            fire_window: Some(MonitorFireWindow {
                start_ms: monitor.fire_window.start_ms,
                count: monitor.fire_window.count as usize,
            }),
            ..Default::default()
        },
        monitor
            .filter
            .as_deref()
            .and_then(crate::shared::safe_reg_exp),
    )
    .with_wakes_per_hour(monitor.wakes_per_hour.map(f64::from));
    if monitor.delivery_paused {
        record.pause();
    }
    manager.bind_monitor_id(&monitor.monitor_id, &id);
    if register(&id, manager.get(&id).expect("restored runtime"), record).is_err() {
        return RestoreOutcome::Lost;
    }
    if monitor.delivery_paused {
        RestoreOutcome::Muted
    } else {
        RestoreOutcome::Restored
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn monitor() -> ManifestMonitor {
        let tmp = std::env::temp_dir();
        let tmp_str = tmp.to_str().unwrap();
        crate::restore::parse_terminal_manifest(&json!({"monitors":[{"monitorId":"mon_saved","sessionId":"s","description":"watch","runtimeKind":"command","durabilityClass":"restartable-command","command":"printf 'restored\\n'","cwd":tmp_str,"createdAt":1,"expiresAt":null,"persistent":true,"suspended":false,"lastCheckpoint":null,"deliveryPaused":true,"fireWindow":{"startMs":1,"count":0}}],"backgroundSessions":[],"updatedAt":1}),"s").unwrap().monitors.remove(0)
    }
    /// A spawned pid does not prove ConPTY's startup handshake and shell init have finished.
    /// `subscribe_output` pairs the retained history with the live stream atomically.
    async fn await_shell_marker(manager: &mut TerminalManager, marker: &str) {
        let (mut observed, mut output) = {
            let runtime = manager.get("bash_1").unwrap();
            runtime.subscribe_output().unwrap()
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !observed.contains(marker) {
                observed.push_str(&output.recv().await.expect("shell ready output"));
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn configured_restore_preserves_shell_and_geometry() {
        let mut saved = monitor();
        // `stty` is absent or does not return under ConPTY. Bash reports the same winsize
        // after one external command once `checkwinsize` is on.
        saved.command = Some(if cfg!(windows) {
            "shopt -s checkwinsize; /usr/bin/true; printf '%s %s\\nshell:%s\\n' \"$LINES\" \"$COLUMNS\" \"${BASH_VERSION:+bash}\"".to_owned()
        } else {
            "stty -echo; stty size; printf 'shell:%s\\n' \"${BASH_VERSION:+bash}\"".to_owned()
        });
        let mut manager = TerminalManager::default();
        let mut settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS;
        settings.default_rows = 33.0;
        settings.default_cols = 91.0;
        let default_shell = maho_tools::shell::get_shell_config(None)
            .ok()
            .map(|s| s.shell);
        let shell = if cfg!(windows) {
            default_shell.as_deref()
        } else {
            Some("/bin/bash")
        };
        assert_eq!(
            restore_configured_command(&saved, &mut manager, shell, &settings, |_, _, _| Ok(())),
            RestoreOutcome::Muted
        );
        await_shell_marker(&mut manager, "33 91\r\nshell:bash\r\n").await;
        let runtime = manager.get("bash_1").unwrap();
        runtime.wait(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(runtime.full_output().unwrap(), "33 91\r\nshell:bash\r\n");
        manager.teardown().unwrap();
    }
    #[tokio::test]
    async fn restores_once_with_stable_identity_and_fresh_muted_runtime() {
        let saved = monitor();
        let mut manager = TerminalManager::default();
        let mut calls = 0;
        assert_eq!(
            restore_command(&saved, &mut manager, |id, _, record| {
                calls += 1;
                assert_eq!(id, "bash_1");
                assert!(record.snapshot.paused);
                assert_eq!(record.snapshot.deadline_ms, None);
                assert_eq!(
                    record.snapshot.fire_window.unwrap().start_ms,
                    saved.fire_window.start_ms
                );
                Ok(())
            }),
            RestoreOutcome::Muted
        );
        assert_eq!(calls, 1);
        assert_eq!(manager.resolve_id("mon_saved"), Some("bash_1".to_owned()));
        await_shell_marker(&mut manager, "restored\r\n").await;
        let runtime = manager.get("bash_1").unwrap();
        runtime.wait(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(runtime.full_output().unwrap(), "restored\r\n");
        manager.teardown().unwrap();
    }
    #[test]
    fn invalid_targets_do_not_spawn() {
        let mut saved = monitor();
        let mut manager = TerminalManager::default();
        saved.cwd = Some("relative".to_owned());
        assert_eq!(
            restore_command(&saved, &mut manager, |_, _, _| panic!(
                "invalid target registered"
            )),
            RestoreOutcome::Lost
        );
        assert_eq!(manager.size(), 0);
        let tmp = std::env::temp_dir();
        saved.cwd = Some(tmp.to_str().unwrap().to_owned());
        saved.persistent = false;
        assert_eq!(
            restore_command(&saved, &mut manager, |_, _, _| panic!(
                "ephemeral target registered"
            )),
            RestoreOutcome::Lost
        );
        assert_eq!(manager.size(), 0);
    }
    #[tokio::test]
    async fn restored_runtime_is_observed_by_live_registry() {
        // `monitor()` prints `restored` with no `stty`. `register` subscribes before the line is required.
        let mut saved = monitor();
        saved.delivery_paused = false;
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let mut registry = crate::monitor_registry::MonitorRegistry::new(move |event| {
            sender.send(event).unwrap();
        });
        let mut manager = TerminalManager::default();
        assert_eq!(
            restore_command(&saved, &mut manager, |_, runtime, record| registry
                .register(runtime, record)),
            RestoreOutcome::Restored
        );
        tokio::time::timeout(std::time::Duration::from_secs(5),async {assert!(matches!(events.recv().await,Some(crate::monitor_registry::MonitorEvent::Line {line,..}) if line=="restored"));assert!(matches!(events.recv().await,Some(crate::monitor_registry::MonitorEvent::Summary {..})));}).await.unwrap();
        manager.teardown().unwrap();
    }
}
