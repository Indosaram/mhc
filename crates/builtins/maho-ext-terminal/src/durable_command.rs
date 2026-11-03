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
    register: impl FnMut(
        &str,
        &crate::runtime_session::TerminalRuntimeSession,
        CommandMonitor,
    ) -> Result<(), crate::runtime_session::RuntimeError>,
) -> RestoreOutcome {
    restore_configured_command_in(
        monitor,
        manager,
        shell,
        settings,
        &RestoreEnvironment::default(),
        register,
    )
}
/// Upstream `restoreEnvironment` inputs: the downtime bound handed to the command and the session's
/// terminal state dir, under which a persistent watch gets its `SENPI_MONITOR_STATE_DIR`.
#[derive(Clone, Debug, Default)]
pub struct RestoreEnvironment {
    pub downtime_ms: f64,
    pub terminal_dir: Option<std::path::PathBuf>,
    /// Upstream `RestoreContext.remainingMs`: time left for an ephemeral watch whose deadline has
    /// not passed; `None` for a durable one.
    pub remaining_ms: Option<f64>,
}
/// Upstream `restoreEnvironment`: stable id, restored flag and downtime bound on every re-spawn, plus
/// the per-monitor state dir (created here) for a persistent watch of a session with a state dir.
pub fn restore_environment(
    monitor: &ManifestMonitor,
    context: &RestoreEnvironment,
) -> std::io::Result<Vec<(String, String)>> {
    use crate::shared::{
        MONITOR_ENV_DOWNTIME_MS, MONITOR_ENV_ID, MONITOR_ENV_RESTORED, MONITOR_ENV_STATE_DIR,
    };
    let mut env = vec![
        (MONITOR_ENV_ID.to_owned(), monitor.monitor_id.clone()),
        (MONITOR_ENV_RESTORED.to_owned(), "1".to_owned()),
        (
            MONITOR_ENV_DOWNTIME_MS.to_owned(),
            format!("{}", context.downtime_ms.max(0.0).trunc() as u64),
        ),
    ];
    if monitor.persistent
        && let Some(terminal_dir) = &context.terminal_dir
    {
        let dir = crate::monitor_state_dir::ensure_monitor_state_dir_sync(
            terminal_dir,
            &monitor.monitor_id,
        )?;
        env.push((
            MONITOR_ENV_STATE_DIR.to_owned(),
            dir.to_string_lossy().into_owned(),
        ));
    }
    Ok(env)
}
pub fn restore_configured_command_in(
    monitor: &ManifestMonitor,
    manager: &mut TerminalManager,
    shell: Option<&str>,
    settings: &crate::settings::ResolvedTerminalSettings,
    context: &RestoreEnvironment,
    mut register: impl FnMut(
        &str,
        &crate::runtime_session::TerminalRuntimeSession,
        CommandMonitor,
    ) -> Result<(), crate::runtime_session::RuntimeError>,
) -> RestoreOutcome {
    // A persistent watch has no deadline; an ephemeral one gets exactly the time it had left.
    let ephemeral_time_left = if monitor.persistent {
        None
    } else {
        context.remaining_ms
    };
    if monitor.runtime_kind != MonitorRuntimeKind::Command
        || (!monitor.persistent && ephemeral_time_left.is_none())
    {
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
    let Ok(env) = restore_environment(monitor, context) else {
        return RestoreOutcome::Lost;
    };
    let mut options = options.size(settings.default_cols as u16, settings.default_rows as u16);
    for (key, value) in env {
        options = options.env(key, value);
    }
    if let Some(time_left) = ephemeral_time_left {
        options = options.timeout(std::time::Duration::from_millis(
            time_left.max(1.0).trunc() as u64
        ));
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0);
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
            persistent: Some(monitor.persistent),
            deadline_ms: ephemeral_time_left.map(|time_left| now + time_left),
            expires_at: monitor.expires_at,
            // Only durable (restartable-command) watches carry the fire window and runaway guard.
            fire_window: monitor.persistent.then_some(MonitorFireWindow {
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
    .with_wakes_per_hour(monitor.wakes_per_hour.map(f64::from))
    .with_human_input(monitor.human_input == Some(true));
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
    /// durable-command.ts `restoreEnvironment`: a re-spawned persistent watch sees its stable id, the
    /// restored flag, the downtime bound and its own state dir (created before the spawn).
    #[cfg(unix)]
    #[tokio::test]
    async fn restore_hands_the_command_its_restore_environment() {
        let terminal_dir = tempfile::tempdir().unwrap();
        let mut saved = monitor();
        saved.command = Some(
            "printf '%s|%s|%s|%s\\n' \"$SENPI_MONITOR_ID\" \"$SENPI_MONITOR_RESTORED\" \"$SENPI_MONITOR_DOWNTIME_MS\" \"$SENPI_MONITOR_STATE_DIR\"".to_owned(),
        );
        let mut manager = TerminalManager::default();
        let context = RestoreEnvironment {
            downtime_ms: 1234.7,
            terminal_dir: Some(terminal_dir.path().to_path_buf()),
            remaining_ms: None,
        };
        assert_eq!(
            restore_configured_command_in(
                &saved,
                &mut manager,
                None,
                &crate::settings::TERMINAL_SETTINGS_DEFAULTS,
                &context,
                |_, _, _| Ok(())
            ),
            RestoreOutcome::Muted
        );
        let state_dir =
            crate::monitor_state_dir::monitor_state_dir(terminal_dir.path(), "mon_saved");
        assert!(state_dir.is_dir());
        let expected = format!("mon_saved|1|1234|{}\r\n", state_dir.display());
        await_shell_marker(&mut manager, &expected).await;
        manager.teardown().unwrap();
    }
    #[test]
    fn restore_environment_omits_the_state_dir_without_a_terminal_dir_or_persistence() {
        let mut saved = monitor();
        let keys =
            |env: Vec<(String, String)>| env.into_iter().map(|(key, _)| key).collect::<Vec<_>>();
        assert_eq!(
            keys(restore_environment(&saved, &RestoreEnvironment::default()).unwrap()),
            [
                crate::shared::MONITOR_ENV_ID,
                crate::shared::MONITOR_ENV_RESTORED,
                crate::shared::MONITOR_ENV_DOWNTIME_MS
            ]
        );
        let terminal_dir = tempfile::tempdir().unwrap();
        saved.persistent = false;
        let context = RestoreEnvironment {
            downtime_ms: 0.0,
            terminal_dir: Some(terminal_dir.path().to_path_buf()),
            remaining_ms: None,
        };
        assert!(
            !keys(restore_environment(&saved, &context).unwrap())
                .contains(&crate::shared::MONITOR_ENV_STATE_DIR.to_owned())
        );
        assert!(
            !crate::monitor_state_dir::monitor_state_dir(terminal_dir.path(), "mon_saved").exists()
        );
    }
    /// restore.ts / durable-command.ts: an ephemeral watch with time left on its deadline is
    /// re-spawned with exactly that time and no fire window; one without stays lost.
    #[tokio::test]
    async fn ephemeral_watch_with_time_left_restores_with_its_remaining_deadline() {
        let mut saved = monitor();
        saved.persistent = false;
        saved.durability_class = crate::terminal_manifest_model::MonitorDurabilityClass::Ephemeral;
        saved.delivery_paused = false;
        let mut manager = TerminalManager::default();
        let context = RestoreEnvironment {
            remaining_ms: Some(60_000.0),
            ..Default::default()
        };
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
            * 1000.0;
        let mut seen = None;
        assert_eq!(
            restore_configured_command_in(
                &saved,
                &mut manager,
                None,
                &crate::settings::TERMINAL_SETTINGS_DEFAULTS,
                &context,
                |_, _, record| {
                    seen = Some(record.snapshot.clone());
                    Ok(())
                }
            ),
            RestoreOutcome::Restored
        );
        let snapshot = seen.unwrap();
        assert_eq!(snapshot.persistent, Some(false));
        assert!(snapshot.fire_window.is_none());
        let deadline = snapshot.deadline_ms.unwrap();
        assert!(deadline >= before + 60_000.0 && deadline <= before + 70_000.0);
        manager.teardown().unwrap();
        let mut manager = TerminalManager::default();
        assert_eq!(
            restore_configured_command_in(
                &saved,
                &mut manager,
                None,
                &crate::settings::TERMINAL_SETTINGS_DEFAULTS,
                &RestoreEnvironment::default(),
                |_, _, _| panic!("an ephemeral watch without time left registered")
            ),
            RestoreOutcome::Lost
        );
        assert_eq!(manager.size(), 0);
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
