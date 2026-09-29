//! CLI contract (cli.ts): `lsp-daemon [mcp | daemon]`, default `mcp`, env overrides.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use lsp_daemon::ensure_daemon::{PROBE_TIMEOUT_MS, probe_daemon};
use lsp_daemon::paths::{
    DaemonPaths, DaemonPlatform, daemon_paths_with, packaged_runtime_defaults,
};
use lsp_daemon::runtime_contract::{
    Env, OMO_LSP_DAEMON_CLI, OMO_LSP_DAEMON_DIR, OMO_LSP_DAEMON_VERSION,
};
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_lsp-daemon");

fn daemon_env(root: &tempfile::TempDir) -> Env {
    [
        (
            OMO_LSP_DAEMON_DIR,
            root.path().to_string_lossy().into_owned(),
        ),
        (OMO_LSP_DAEMON_CLI, BIN.to_string()),
        (OMO_LSP_DAEMON_VERSION, "9.9.9-cli".to_string()),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value))
    .collect()
}

fn paths_for(env: &Env) -> DaemonPaths {
    daemon_paths_with(
        env,
        &packaged_runtime_defaults(),
        &DaemonPlatform::current(),
    )
    .unwrap()
}

fn command(args: &[&str], env: &Env) -> Command {
    let mut command = Command::new(BIN);
    command
        .args(args)
        .envs(env)
        .env_remove("LSP_TOOLS_MCP_PROJECT_CONFIG");
    command
}

#[test]
fn unknown_subcommand_prints_usage_and_exits_2() {
    let output = command(&["bogus"], &Env::new()).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "Usage: omo-lsp-daemon [mcp | daemon]\n"
    );
}

fn initialize_via(args: &[&str]) -> Value {
    let root = common::short_tempdir();
    let mut child = command(args, &daemon_env(&root))
        .current_dir(root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    drop(stdin);
    assert!(child.wait().unwrap().success());
    serde_json::from_str(&line).unwrap()
}

#[test]
fn default_subcommand_is_the_mcp_proxy() {
    let response = initialize_via(&[]);
    assert_eq!(response["result"]["serverInfo"]["name"], json!("lsp"));
}

#[test]
fn mcp_subcommand_serves_initialize_locally() {
    let response = initialize_via(&["mcp"]);
    assert_eq!(response["id"], json!(1));
    assert!(response["result"]["capabilities"]["tools"].is_object());
}

#[test]
fn singleton_runtime_override_fails_with_invalid_runtime_override() {
    let root = common::short_tempdir();
    let mut env = daemon_env(&root);
    env.remove(OMO_LSP_DAEMON_VERSION);
    let output = command(&["daemon"], &env).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains(OMO_LSP_DAEMON_CLI));
}

fn wait_for_probe(paths: &DaemonPaths) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(common::within(async {
        while !probe_daemon(paths, PROBE_TIMEOUT_MS, None).await {
            tokio::task::yield_now().await;
        }
    }));
}

#[cfg(unix)]
fn terminate(child: &mut std::process::Child) -> std::process::ExitStatus {
    // SAFETY: signalling our own child process by pid.
    unsafe {
        libc::kill(i32::try_from(child.id()).unwrap(), libc::SIGTERM);
    }
    child.wait().unwrap()
}

#[cfg(unix)]
#[test]
fn daemon_subcommand_reclaims_a_crashed_owner_and_exits_cleanly_on_sigterm() {
    let root = common::short_tempdir();
    let env = daemon_env(&root);
    let paths = paths_for(&env);
    assert!(paths.dir.ends_with("v9.9.9-cli"));
    // Leftovers of a crashed daemon: lock and pid naming a dead process.
    std::fs::create_dir_all(&paths.dir).unwrap();
    std::fs::write(&paths.lock, "9999999").unwrap();
    std::fs::write(&paths.pid, "9999999").unwrap();
    let mut child = command(&["daemon"], &env)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for_probe(&paths);
    assert_eq!(
        std::fs::read_to_string(&paths.pid).unwrap().trim(),
        child.id().to_string()
    );

    // A second daemon for the same version sees the live owner and exits 0.
    let second = command(&["daemon"], &env).output().unwrap();
    assert_eq!(second.status.code(), Some(0));

    assert!(terminate(&mut child).success());
    assert!(!paths.socket.exists());
    assert!(!paths.pid.exists());
}
