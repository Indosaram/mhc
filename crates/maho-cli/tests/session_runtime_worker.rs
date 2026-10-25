//! senpi main.ts:1119-1148 `resolveSessionRuntime`: a stdio multi-session host runs every session in
//! its own isolated worker process, while `--session-runtime in-process` keeps them in the host.
//! Drives the real binary over its stdio protocol and observes the host's child processes.
#![cfg(target_os = "linux")]

use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

const STEP: Duration = Duration::from_secs(60);

struct Host {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    lines: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    _home: tempfile::TempDir,
    cwd: String,
}

impl Host {
    fn start(extra: &[&str]) -> Self {
        let home = tempfile::tempdir().expect("isolated home");
        let cwd = home.path().join("project");
        std::fs::create_dir_all(&cwd).expect("project dir");
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_mhc"))
            .current_dir(&cwd)
            .env("HOME", home.path())
            .env("PI_OFFLINE", "1")
            .env_remove("__PI_INTERNAL_SPAWN")
            .args(["--mode", "rpc", "--multi-session", "--no-extensions"])
            .args(extra)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("start the real host");
        let stdin = child.stdin.take().expect("piped stdin");
        let lines = tokio::io::BufReader::new(child.stdout.take().expect("piped stdout")).lines();
        Self {
            child,
            stdin,
            lines,
            _home: home,
            cwd: cwd
                .canonicalize()
                .expect("cwd")
                .to_string_lossy()
                .into_owned(),
        }
    }

    /// Sends one command and awaits exactly its response record; other records are skipped.
    async fn call(&mut self, command: serde_json::Value) -> serde_json::Value {
        let kind = command["type"].as_str().expect("command type").to_owned();
        self.stdin
            .write_all(format!("{command}\n").as_bytes())
            .await
            .expect("write command");
        tokio::time::timeout(STEP, async {
            loop {
                let line = self
                    .lines
                    .next_line()
                    .await
                    .expect("read host")
                    .expect("host closed stdout");
                let record: serde_json::Value = serde_json::from_str(&line).expect("json record");
                if record["type"] == "response" && record["command"] == kind.as_str() {
                    return record;
                }
            }
        })
        .await
        .expect("the host answered in time")
    }

    fn pid(&self) -> u32 {
        self.child.id().expect("host is running")
    }
}

/// Direct children of `pid`, across every thread of the process.
fn children(pid: u32) -> Vec<u32> {
    let mut children = Vec::new();
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return children;
    };
    for task in tasks.flatten() {
        let listed = std::fs::read_to_string(task.path().join("children")).unwrap_or_default();
        children.extend(
            listed
                .split_whitespace()
                .filter_map(|value| value.parse::<u32>().ok()),
        );
    }
    children
}

fn role_of(pid: u32) -> Option<String> {
    let environ = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    environ.split(|byte| *byte == 0).find_map(|entry| {
        std::str::from_utf8(entry)
            .ok()?
            .strip_prefix("__PI_INTERNAL_SPAWN=")
            .map(str::to_owned)
    })
}

#[tokio::test]
async fn a_stdio_host_runs_each_session_in_its_own_worker_process() {
    let mut host = Host::start(&[]);
    let info = host
        .call(serde_json::json!({ "type": "get_protocol_info" }))
        .await;
    assert_eq!(
        info["data"]["launch_profile"]["core"]["session_runtime"], "worker",
        "{info}"
    );

    let cwd = host.cwd.clone();
    let opened = host
        .call(serde_json::json!({ "type": "open_session", "cwd": cwd }))
        .await;
    assert_eq!(opened["success"], true, "{opened}");
    let handle = opened["data"]["sessionId"]
        .as_str()
        .expect("routing handle")
        .to_owned();
    assert_eq!(opened["data"]["state"]["cwd"], cwd.as_str(), "{opened}");

    let workers = children(host.pid())
        .into_iter()
        .filter(|pid| role_of(*pid).as_deref() == Some("rpc-session-worker"))
        .collect::<Vec<_>>();
    assert_eq!(workers.len(), 1, "one isolated worker per open session");

    let state = host
        .call(serde_json::json!({ "type": "get_state", "sessionId": handle }))
        .await;
    assert_eq!(state["success"], true, "{state}");
    assert_eq!(state["sessionId"], handle.as_str());
    assert_eq!(state["data"]["cwd"], cwd.as_str(), "{state}");

    let listed = host
        .call(serde_json::json!({ "type": "list_sessions", "include_workers": true }))
        .await;
    assert_eq!(
        listed["data"]["sessions"][0]["sessionId"],
        handle.as_str(),
        "{listed}"
    );
    assert_eq!(listed["data"]["sessions"][0]["status"], "open", "{listed}");

    let closed = host
        .call(serde_json::json!({ "type": "close_session", "sessionId": handle }))
        .await;
    assert_eq!(closed["success"], true, "{closed}");
    // The close answers only after the worker's actual exit, which the host has reaped.
    assert!(
        !children(host.pid()).contains(&workers[0]),
        "the worker exited with its session"
    );
    let listed = host
        .call(serde_json::json!({ "type": "list_sessions", "include_workers": true }))
        .await;
    assert_eq!(
        listed["data"]["sessions"],
        serde_json::json!([]),
        "{listed}"
    );
}

#[tokio::test]
async fn an_in_process_host_keeps_sessions_in_the_host_process() {
    let mut host = Host::start(&["--session-runtime", "in-process"]);
    let info = host
        .call(serde_json::json!({ "type": "get_protocol_info" }))
        .await;
    assert_eq!(
        info["data"]["launch_profile"]["core"]["session_runtime"], "in-process",
        "{info}"
    );
    let cwd = host.cwd.clone();
    let opened = host
        .call(serde_json::json!({ "type": "open_session", "cwd": cwd }))
        .await;
    assert_eq!(opened["success"], true, "{opened}");
    assert!(
        !children(host.pid())
            .into_iter()
            .any(|pid| role_of(pid).as_deref() == Some("rpc-session-worker")),
        "an in-process session spawns no worker"
    );
}

#[tokio::test]
async fn real_host_republishes_terminal_then_running_task_context_without_changing_identity() {
    let mut host = Host::start(&["--session-runtime", "in-process"]);
    let cwd = host.cwd.clone();
    let opened = host
        .call(serde_json::json!({"type":"open_session","cwd":cwd,"kind":"worker"}))
        .await;
    assert_eq!(opened["success"], true, "{opened}");
    let id = opened["data"]["sessionId"].as_str().unwrap().to_owned();
    for status in ["completed", "running", "error"] {
        let published = host.call(serde_json::json!({"type":"set_session_context","sessionId":id,"patch":{"task_status":status}})).await;
        assert_eq!(published["success"], true, "{published}");
        let listed = host
            .call(serde_json::json!({"type":"list_sessions","include_workers":true}))
            .await;
        let row = &listed["data"]["sessions"][0];
        assert_eq!(row["sessionId"], id);
        assert_eq!(row["context"]["task_status"], status, "{listed}");
    }
    assert_eq!(
        host.call(serde_json::json!({"type":"close_session","sessionId":id}))
            .await["success"],
        true
    );
}
