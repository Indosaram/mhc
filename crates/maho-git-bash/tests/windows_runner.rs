//! Acceptance test 10 of `docs/cleanroom/specs/git-bash-mcp.md` §8 (W): the real runner on a
//! Windows host with Git Bash installed.
#![cfg(windows)]

use maho_git_bash::{Host, Server, ServerOptions, SystemHost, resolve};
use serde_json::{Value, json};
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(60);

struct Fixture {
    temp_root: tempfile::TempDir,
    server: Server<SystemHost>,
}

fn fixture() -> Fixture {
    let temp_root = tempfile::tempdir().expect("temp root");
    let host = SystemHost {
        temp_root: Some(temp_root.path().to_path_buf()),
    };
    assert!(
        resolve(&host).is_found(),
        "these tests need Git for Windows: {}",
        resolve(&host).to_json()
    );
    Fixture {
        temp_root,
        server: Server::new(host, ServerOptions::default()),
    }
}

impl Fixture {
    async fn run(&self, arguments: Value) -> Value {
        let reply = tokio::time::timeout(BOUND, self.server.call_tool("run", Some(&arguments)))
            .await
            .expect("run finished within bound");
        assert!(!reply.is_error, "{}", reply.text);
        serde_json::from_str(&reply.text).expect("run JSON")
    }

    fn assert_no_leftover_output_dirs(&self) {
        let left: Vec<_> = std::fs::read_dir(self.temp_root.path())
            .expect("read temp root")
            .collect();
        assert!(left.is_empty(), "per-call temp directories left: {left:?}");
    }
}

#[tokio::test]
async fn t10_stdout_and_stderr_are_separate() {
    let fixture = fixture();
    let value = fixture.run(json!({"command": "echo hi; echo err >&2"})).await;
    assert_eq!(value["stdout"], "hi\n");
    assert_eq!(value["stderr"], "err\n");
    assert_eq!(value["exitCode"], 0);
    assert_eq!(value["timedOut"], false);
    fixture.assert_no_leftover_output_dirs();
}

#[tokio::test]
async fn t10_timeout_kills_the_command() {
    let fixture = fixture();
    let value = fixture
        .run(json!({"command": "echo started; sleep 5", "timeout": 200}))
        .await;
    assert_eq!(value["timedOut"], true);
    fixture.assert_no_leftover_output_dirs();
}

#[tokio::test]
async fn t10_workdir_is_honoured() {
    let fixture = fixture();
    let workdir = tempfile::Builder::new()
        .prefix("git-bash-workdir-")
        .tempdir()
        .expect("workdir");
    let name = workdir
        .path()
        .file_name()
        .expect("dir name")
        .to_string_lossy()
        .into_owned();
    let value = fixture
        .run(json!({"command": "pwd", "workdir": workdir.path().to_string_lossy()}))
        .await;
    let stdout = value["stdout"].as_str().expect("stdout");
    assert!(stdout.trim_end().ends_with(&name), "{stdout}");
}

#[tokio::test]
async fn t10_large_output_is_returned_whole() {
    let fixture = fixture();
    let size = 3 * 1024 * 1024;
    let value = fixture
        .run(json!({"command": format!("head -c {size} /dev/zero | tr '\\0' 'a'")}))
        .await;
    let stdout = value["stdout"].as_str().expect("stdout");
    assert_eq!(stdout.len(), size);
    assert!(stdout.bytes().all(|byte| byte == b'a'));
    fixture.assert_no_leftover_output_dirs();
}

#[tokio::test]
async fn t10_non_zero_exit_code_is_reported() {
    let fixture = fixture();
    let value = fixture.run(json!({"command": "echo out; exit 3"})).await;
    assert_eq!(value["exitCode"], 3);
    assert_eq!(value["stdout"], "out\n");
}

#[test]
fn t10_system_host_reports_win32() {
    assert_eq!(SystemHost::default().platform(), "win32");
}
