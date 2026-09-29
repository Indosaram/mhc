use super::*;
use pretty_assertions::assert_eq;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

struct Harness {
    deps: EnsureDaemonDeps,
    probes: Arc<AtomicUsize>,
    spawns: Arc<AtomicUsize>,
    sleeps: Arc<AtomicUsize>,
}

/// Fake deps: probes answer from `queue` (then `fallback`), time advances 100ms per sleep.
fn harness(queue: Vec<bool>, fallback: bool) -> Harness {
    let queue = Arc::new(Mutex::new(VecDeque::from(queue)));
    let probes = Arc::new(AtomicUsize::new(0));
    let spawns = Arc::new(AtomicUsize::new(0));
    let sleeps = Arc::new(AtomicUsize::new(0));
    let clock = Arc::new(AtomicU64::new(0));
    let deps = EnsureDaemonDeps {
        probe: Arc::new({
            let (queue, probes) = (Arc::clone(&queue), Arc::clone(&probes));
            move |_paths, _signal| {
                probes.fetch_add(1, Ordering::SeqCst);
                let answer = queue.lock().unwrap().pop_front().unwrap_or(fallback);
                Box::pin(async move { answer })
            }
        }),
        spawn_daemon: Arc::new({
            let spawns = Arc::clone(&spawns);
            move |_paths| {
                spawns.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }),
        sleep: Arc::new({
            let (sleeps, clock) = (Arc::clone(&sleeps), Arc::clone(&clock));
            move |ms, _signal| {
                sleeps.fetch_add(1, Ordering::SeqCst);
                clock.fetch_add(ms, Ordering::SeqCst);
                Box::pin(async {})
            }
        }),
        now: Arc::new(move || clock.load(Ordering::SeqCst)),
    };
    Harness {
        deps,
        probes,
        spawns,
        sleeps,
    }
}

fn paths(dir: &tempfile::TempDir) -> DaemonPaths {
    DaemonPaths::under_dir(dir.path().join("v1"), "1")
}

#[test]
fn removed_cached_node_falls_back_to_argv0() {
    let executable = resolve_daemon_node_executable(
        "/opt/homebrew/Cellar/node/26.5.0/bin/node",
        "/opt/homebrew/bin/node",
        |path| path == "/opt/homebrew/bin/node",
    );
    assert_eq!(executable, "/opt/homebrew/bin/node");
}

#[test]
fn no_absolute_launcher_uses_path_node() {
    assert_eq!(
        resolve_daemon_node_executable("/removed/node", "node", |_| false),
        "node"
    );
}

#[test]
fn existing_cached_node_is_preserved() {
    assert_eq!(
        resolve_daemon_node_executable("/runtime/node", "node", |path| path == "/runtime/node"),
        "/runtime/node"
    );
}

#[tokio::test]
async fn reachable_daemon_is_not_spawned() {
    let dir = tempfile::tempdir().unwrap();
    let harness = harness(vec![true], false);
    ensure_daemon_running(&paths(&dir), &harness.deps, EnsureDaemonOptions::default())
        .await
        .unwrap();
    assert_eq!(harness.spawns.load(Ordering::SeqCst), 0);
    assert_eq!(harness.probes.load(Ordering::SeqCst), 1);
    assert!(!paths(&dir).lock.exists());
}

#[tokio::test]
async fn not_running_spawns_without_owning_the_lock_and_waits() {
    let dir = tempfile::tempdir().unwrap();
    let harness = harness(vec![false, false, true], false);
    ensure_daemon_running(&paths(&dir), &harness.deps, EnsureDaemonOptions::default())
        .await
        .unwrap();
    assert_eq!(harness.spawns.load(Ordering::SeqCst), 1);
    assert_eq!(harness.probes.load(Ordering::SeqCst), 3);
    assert_eq!(harness.sleeps.load(Ordering::SeqCst), 1);
    assert!(!paths(&dir).lock.exists());
}

#[tokio::test]
async fn another_candidate_winning_is_observed_by_polling() {
    let dir = tempfile::tempdir().unwrap();
    let harness = harness(vec![false, false, false, true], false);
    ensure_daemon_running(&paths(&dir), &harness.deps, EnsureDaemonOptions::default())
        .await
        .unwrap();
    assert_eq!(harness.spawns.load(Ordering::SeqCst), 1);
    assert_eq!(harness.sleeps.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn never_reachable_spawn_throws_unreachable() {
    let dir = tempfile::tempdir().unwrap();
    let harness = harness(Vec::new(), false);
    let options = EnsureDaemonOptions {
        ready_timeout_ms: Some(300),
        poll_interval_ms: Some(100),
        signal: None,
    };
    let error = ensure_daemon_running(&paths(&dir), &harness.deps, options)
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "LSP daemon did not become reachable at {}",
            paths(&dir).socket.display()
        )
    );
    assert_eq!(harness.spawns.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn pending_probe_abort_settles_without_spawning() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(Vec::new(), false);
    harness.deps.probe = Arc::new(|_paths, _signal| Box::pin(std::future::pending()));
    let controller = lsp_core::abort::AbortController::new();
    let options = EnsureDaemonOptions {
        signal: Some(controller.signal()),
        ..EnsureDaemonOptions::default()
    };
    let paths = paths(&dir);
    let running = tokio::spawn({
        let deps = harness.deps.clone();
        async move { ensure_daemon_running(&paths, &deps, options).await }
    });
    tokio::task::yield_now().await;
    controller.abort();
    let result = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Err(EnsureDaemonError::Aborted(_))));
    assert_eq!(harness.spawns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn aborted_probe_with_missing_socket_stays_contained() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    std::fs::create_dir_all(&paths.dir).unwrap();
    std::fs::write(&paths.auth, "token").unwrap();
    let controller = lsp_core::abort::AbortController::new();
    controller.abort();
    assert!(!probe_daemon(&paths, PROBE_TIMEOUT_MS, Some(controller.signal())).await);
    assert!(!probe_daemon(&paths, PROBE_TIMEOUT_MS, None).await);
}

#[test]
fn native_binary_and_node_script_launch_commands() {
    let (program, args) = daemon_launch_command(Path::new("/opt/omo/bin/lsp-daemon"));
    assert_eq!(program, PathBuf::from("/opt/omo/bin/lsp-daemon"));
    assert_eq!(args, vec![PathBuf::from("daemon")]);
    let (program, args) = daemon_launch_command(Path::new("/opt/omo/dist/cli.js"));
    assert_eq!(program, PathBuf::from("node"));
    assert_eq!(
        args,
        vec![
            PathBuf::from("/opt/omo/dist/cli.js"),
            PathBuf::from("daemon")
        ]
    );
}
