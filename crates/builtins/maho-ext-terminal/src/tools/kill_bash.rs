use super::context::{TerminalToolResult, error_result, resolve_terminal_id, text_result};
use crate::manager::TerminalManager;
pub fn kill_bash_schema() -> serde_json::Value {
    serde_json::json!({"type":"object","properties":{"bash_id":{"type":"string","description":"Session id to tree-kill."},"all":{"type":"boolean","description":"Tree-kill every live background session."}},"required":[]})
}
pub fn execute_kill_bash(
    manager: &mut TerminalManager,
    id: Option<&str>,
    all: bool,
) -> TerminalToolResult {
    if all {
        let count = manager.size();
        return match manager.teardown() {
            Ok(()) => text_result(format!("Killed {count} session(s).")),
            Err(error) => error_result(error.to_string()),
        };
    }
    let Some(id) = id.filter(|id| !id.is_empty()) else {
        return error_result("Provide `bash_id` or set `all:true`.");
    };
    let resolved = resolve_terminal_id(manager, id);
    match manager.stop(&resolved) {
        Ok(true) => text_result(format!("Killed {id}.")),
        Ok(false) => error_result(format!("No terminal session found with id: {id}")),
        Err(error) => error_result(error.to_string()),
    }
}
/// `execute_kill_bash` for a shared manager, as kill-bash.ts awaits it: the stop/teardown exit
/// graces are awaited with the manager lock released, never blocking the async runtime thread.
pub async fn execute_kill_bash_shared(
    manager: &std::sync::Arc<std::sync::Mutex<TerminalManager>>,
    id: Option<&str>,
    all: bool,
) -> TerminalToolResult {
    if all {
        let count = manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .size();
        return match crate::manager::teardown_shared(manager).await {
            Ok(()) => text_result(format!("Killed {count} session(s).")),
            Err(error) => error_result(error.to_string()),
        };
    }
    let Some(id) = id.filter(|id| !id.is_empty()) else {
        return error_result("Provide `bash_id` or set `all:true`.");
    };
    let resolved = resolve_terminal_id(
        &manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        id,
    );
    match crate::manager::stop_shared(manager, &resolved).await {
        Ok(true) => text_result(format!("Killed {id}.")),
        Ok(false) => error_result(format!("No terminal session found with id: {id}")),
        Err(error) => error_result(error.to_string()),
    }
}
/// kill-bash.ts `execute` with the monitor registry and the session's terminal state dir: file
/// watches are stopped through the registry, `all` also counts and stops every file watch, and an
/// explicit kill ends a standing watch for good, so its restore state dir goes with it (a process
/// death does not come through here and keeps the dir for the next restore).
pub async fn execute_kill_bash_with_monitors(
    manager: &std::sync::Arc<std::sync::Mutex<TerminalManager>>,
    monitors: &std::sync::Arc<std::sync::Mutex<crate::monitor_registry::MonitorRegistry>>,
    terminal_dir: Option<&std::path::Path>,
    id: Option<&str>,
    all: bool,
) -> TerminalToolResult {
    let drop_state_dir = |monitor_id: Option<&str>| {
        if let (Some(dir), Some(monitor_id)) = (terminal_dir, monitor_id)
            && let Err(error) =
                crate::monitor_state_dir::remove_monitor_state_dir_sync(dir, monitor_id)
        {
            maho_ext_api::diagnostic(&format!(
                "Could not remove the state dir of {monitor_id}: {error}"
            ));
        }
    };
    if all {
        let terminal_count = manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .size();
        // Upstream `stopAllFiles` + `stopAllRpc`: every file watch (and every registration still
        // proving its target) and every rpc watch ends. They run the monitor-ended callbacks, so
        // the manager lock is not held across them.
        let (monitor_ids, file_count, rpc_count) = {
            let mut monitors = monitors
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let monitor_ids = monitors
                .snapshot()
                .iter()
                .filter_map(|entry| entry.monitor_id.clone())
                .collect::<Vec<_>>();
            let file_count = monitors.stop_all_files();
            let rpc_count = monitors.stop_all_rpc();
            (monitor_ids, file_count, rpc_count)
        };
        if let Err(error) = crate::manager::teardown_shared(manager).await {
            return error_result(error.to_string());
        }
        for monitor_id in &monitor_ids {
            drop_state_dir(Some(monitor_id));
        }
        return text_result(format!(
            "Killed {} session(s).",
            terminal_count + file_count + rpc_count
        ));
    }
    let Some(id) = id.filter(|id| !id.is_empty()) else {
        return error_result("Provide `bash_id` or set `all:true`.");
    };
    let (resolved, monitor_id) = {
        let manager = manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let resolved = resolve_terminal_id(&manager, id);
        let monitor_id = if id.starts_with("mon_") {
            Some(id.to_owned())
        } else {
            manager.monitor_id_of(&resolved)
        };
        (resolved, monitor_id)
    };
    let stopped = {
        let mut monitors = monitors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        monitors.stop_file(&resolved) || monitors.stop_rpc(&resolved)
    };
    if stopped {
        drop_state_dir(monitor_id.as_deref());
        return text_result(format!("Killed {id}."));
    }
    match crate::manager::stop_shared(manager, &resolved).await {
        Ok(true) => {
            drop_state_dir(monitor_id.as_deref());
            text_result(format!("Killed {id}."))
        }
        Ok(false) => error_result(format!("No terminal session found with id: {id}")),
        Err(error) => error_result(error.to_string()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    type Shared<T> = std::sync::Arc<std::sync::Mutex<T>>;
    fn shared_fixture() -> (
        Shared<TerminalManager>,
        Shared<crate::monitor_registry::MonitorRegistry>,
        tempfile::TempDir,
    ) {
        (
            std::sync::Arc::new(std::sync::Mutex::new(TerminalManager::default())),
            std::sync::Arc::new(std::sync::Mutex::new(
                crate::monitor_registry::MonitorRegistry::new(|_| {}),
            )),
            tempfile::tempdir().unwrap(),
        )
    }
    #[tokio::test]
    async fn explicit_kill_drops_the_watch_state_dir_by_monitor_or_runtime_id() {
        let (manager, monitors, terminal) = shared_fixture();
        let dir = terminal.path();
        let mut ids = vec![];
        for monitor_id in ["mon_by_monitor", "mon_by_runtime"] {
            let id = manager
                .lock()
                .unwrap()
                .create("read", crate::shared::test_sh_options("read value"))
                .unwrap();
            manager.lock().unwrap().bind_monitor_id(monitor_id, &id);
            crate::monitor_state_dir::ensure_monitor_state_dir_sync(dir, monitor_id).unwrap();
            ids.push(id);
        }
        let killed = execute_kill_bash_with_monitors(
            &manager,
            &monitors,
            Some(dir),
            Some("mon_by_monitor"),
            false,
        )
        .await;
        assert_eq!(killed.content[0].text, "Killed mon_by_monitor.");
        assert!(!crate::monitor_state_dir::monitor_state_dir(dir, "mon_by_monitor").exists());
        assert!(crate::monitor_state_dir::monitor_state_dir(dir, "mon_by_runtime").exists());
        let killed =
            execute_kill_bash_with_monitors(&manager, &monitors, Some(dir), Some(&ids[1]), false)
                .await;
        assert_eq!(killed.content[0].text, format!("Killed {}.", ids[1]));
        assert!(!crate::monitor_state_dir::monitor_state_dir(dir, "mon_by_runtime").exists());
        assert!(
            manager
                .lock()
                .unwrap()
                .get(&ids[0])
                .unwrap()
                .exited()
                .unwrap()
        );
        // Without a session state dir (print/json mode) the kill still succeeds.
        crate::manager::teardown_shared(&manager).await.unwrap();
    }
    #[tokio::test]
    async fn file_watch_is_killed_through_the_registry_and_all_counts_files_and_terminals() {
        let (manager, monitors, terminal) = shared_fixture();
        let dir = terminal.path();
        let (watch, monitor_id) = monitors
            .lock()
            .unwrap()
            .register_file(
                "artifact",
                &dir.join("artifact"),
                crate::terminal_manifest_model::FileEvent::Create,
                60_000,
            )
            .unwrap();
        manager.lock().unwrap().bind_monitor_id(&monitor_id, &watch);
        crate::monitor_state_dir::ensure_monitor_state_dir_sync(dir, &monitor_id).unwrap();
        let killed =
            execute_kill_bash_with_monitors(&manager, &monitors, Some(dir), Some(&watch), false)
                .await;
        assert_eq!(killed.content[0].text, format!("Killed {watch}."));
        assert!(monitors.lock().unwrap().snapshot().is_empty());
        assert!(!crate::monitor_state_dir::monitor_state_dir(dir, &monitor_id).exists());
        // A killed watch is gone: a second kill reports it missing.
        let again =
            execute_kill_bash_with_monitors(&manager, &monitors, Some(dir), Some(&watch), false)
                .await;
        assert_eq!(again.is_error, Some(true));
        assert_eq!(
            again.content[0].text,
            format!("No terminal session found with id: {watch}")
        );

        let (second, second_monitor) = monitors
            .lock()
            .unwrap()
            .register_file(
                "second",
                &dir.join("second"),
                crate::terminal_manifest_model::FileEvent::Create,
                60_000,
            )
            .unwrap();
        crate::monitor_state_dir::ensure_monitor_state_dir_sync(dir, &second_monitor).unwrap();
        manager
            .lock()
            .unwrap()
            .create("read", crate::shared::test_sh_options("read value"))
            .unwrap();
        let all = execute_kill_bash_with_monitors(&manager, &monitors, Some(dir), None, true).await;
        assert_eq!(all.content[0].text, "Killed 2 session(s).");
        assert!(monitors.lock().unwrap().snapshot().is_empty());
        assert_eq!(manager.lock().unwrap().size(), 0);
        assert!(!crate::monitor_state_dir::monitor_state_dir(dir, &second_monitor).exists());
        assert_eq!(manager.lock().unwrap().resolve_id(&second_monitor), None);
        let _ = second;
        let missing = execute_kill_bash_with_monitors(&manager, &monitors, None, None, false).await;
        assert_eq!(
            missing.content[0].text,
            "Provide `bash_id` or set `all:true`."
        );
        let empty =
            execute_kill_bash_with_monitors(&manager, &monitors, None, Some(""), false).await;
        assert_eq!(empty.is_error, Some(true));
    }
    /// kill-bash.ts `stopRpc` / `stopAllRpc`: an rpc watch is killed by its runtime id and by
    /// `all`, which counts it with the terminals and file watches.
    #[tokio::test]
    async fn rpc_watches_are_killed_by_id_and_counted_by_all() {
        let (manager, monitors, terminal) = shared_fixture();
        let arm = |monitor_id: &str| {
            let source = monitors.lock().unwrap().rpc_source();
            let (id, _, _start) = source
                .arm(crate::rpc_monitor_source::RegisterRpcMonitorOptions {
                    description: "kids".into(),
                    target: crate::rpc_monitor_targets::RpcTarget::All,
                    target_text: "all".into(),
                    events: crate::rpc_monitor_targets::RpcEvents::Kinds(vec![
                        "session_status".into(),
                    ]),
                    exclude: vec![],
                    filter: None,
                    persistent: true,
                    timeout_ms: None,
                    expires_at: None,
                    monitor_id: Some(monitor_id.to_owned()),
                    agent_dir: terminal.path().into(),
                    endpoints: Some(std::sync::Arc::new(
                        || -> crate::rpc_monitor_watch::BoxFuture<Vec<String>> {
                            Box::pin(async { vec![] })
                        },
                    )),
                    opener: None,
                    restored_from: None,
                    on_checkpoint: None,
                })
                .unwrap();
            id
        };
        let first = arm("mon_RPCKILL00000001");
        let killed = execute_kill_bash_with_monitors(
            &manager,
            &monitors,
            Some(terminal.path()),
            Some(&first),
            false,
        )
        .await;
        assert_eq!(killed.content[0].text, format!("Killed {first}."));
        assert!(monitors.lock().unwrap().snapshot().is_empty());
        arm("mon_RPCKILL00000002");
        let all =
            execute_kill_bash_with_monitors(&manager, &monitors, Some(terminal.path()), None, true)
                .await;
        assert_eq!(all.content[0].text, "Killed 1 session(s).");
        assert!(monitors.lock().unwrap().snapshot().is_empty());
    }
    #[test]
    fn stable_monitor_id_kills_live_runtime_and_all_clears_bindings() {
        let mut manager = TerminalManager::default();
        let id = manager
            .create("read", crate::shared::test_sh_options("read value"))
            .unwrap();
        manager.bind_monitor_id("mon_saved", &id);
        assert_eq!(
            execute_kill_bash(&mut manager, None, false).is_error,
            Some(true)
        );
        assert_eq!(
            execute_kill_bash(&mut manager, Some("missing"), false).is_error,
            Some(true)
        );
        assert_eq!(
            execute_kill_bash(&mut manager, Some("mon_saved"), false).is_error,
            None
        );
        assert!(manager.get(&id).unwrap().exited().unwrap());
        assert_eq!(manager.active_size().unwrap(), 0);
        assert_eq!(execute_kill_bash(&mut manager, None, true).is_error, None);
        assert_eq!(manager.size(), 0);
        assert_eq!(manager.resolve_id("mon_saved"), None);
    }
    #[tokio::test]
    async fn shared_kill_awaits_stop_without_holding_the_manager_lock() {
        let manager = std::sync::Arc::new(std::sync::Mutex::new(TerminalManager::default()));
        let id = manager
            .lock()
            .unwrap()
            .create("read", crate::shared::test_sh_options("read value"))
            .unwrap();
        manager.lock().unwrap().bind_monitor_id("mon_saved", &id);
        assert_eq!(
            execute_kill_bash_shared(&manager, Some("missing"), false)
                .await
                .is_error,
            Some(true)
        );
        assert_eq!(
            execute_kill_bash_shared(&manager, Some("mon_saved"), false)
                .await
                .is_error,
            None
        );
        assert!(manager.lock().unwrap().get(&id).unwrap().exited().unwrap());
        assert_eq!(
            execute_kill_bash_shared(&manager, None, true)
                .await
                .is_error,
            None
        );
        assert_eq!(manager.lock().unwrap().size(), 0);
    }
    #[tokio::test]
    async fn shared_kill_recovers_a_poisoned_manager() {
        let manager = std::sync::Arc::new(std::sync::Mutex::new(TerminalManager::default()));
        let id = manager
            .lock()
            .unwrap()
            .create(
                "read",
                crate::shared::test_sh_options("printf 'kill-ready\\n'; read value"),
            )
            .unwrap();
        let (mut exit, mut observed, mut output) = {
            let mut manager = manager.lock().unwrap();
            let runtime = manager.get(&id).unwrap();
            let (history, output) = runtime.subscribe_output().unwrap();
            (runtime.subscribe_exit(), history, output)
        };
        // A spawned PID does not prove that ConPTY's startup handshake and the shell's
        // initialization have finished. Kill only after observing the live shell, not
        // while its terminal is still starting. History and future output are atomic.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !observed.contains("kill-ready") {
                observed.push_str(&output.recv().await.expect("shell ready output"));
            }
        })
        .await
        .unwrap();
        let poisoner = manager.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = poisoner.lock().unwrap();
                panic!("poison the manager");
            })
            .join()
            .is_err()
        );
        assert!(manager.is_poisoned());
        assert_eq!(
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                execute_kill_bash_shared(&manager, Some(&id), false)
            )
            .await
            .unwrap()
            .is_error,
            None
        );
        let settled = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            exit.wait_for(Option::is_some),
        )
        .await
        .unwrap()
        .unwrap()
        .clone()
        .unwrap()
        .unwrap();
        assert!(settled.cancelled);
        assert_eq!(
            execute_kill_bash_shared(&manager, None, true)
                .await
                .is_error,
            None
        );
        assert_eq!(
            manager
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .size(),
            0
        );
    }
}
