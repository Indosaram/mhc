//! Acceptance tests 1, 2, 3 and 9 of `docs/cleanroom/specs/git-bash-mcp.md` §8: command line,
//! serving lifecycle and the JSON-RPC surface.

mod common;

use common::{FakeHost, server};
use maho_git_bash::protocol::handle_line;
use maho_git_bash::{CliCommand, Server, USAGE, parse_args, run_cli};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream};

const BOUND: Duration = Duration::from_secs(30);

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| (*arg).to_owned()).collect()
}

/// A client connected to a server running `run_cli` on in-memory pipes.
struct Client {
    to_server: Option<DuplexStream>,
    from_server: BufReader<DuplexStream>,
}

impl Client {
    async fn send(&mut self, line: &str) {
        let writer = self.to_server.as_mut().expect("stdin open");
        writer
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("write request");
    }

    async fn reply(&mut self) -> Value {
        let mut line = String::new();
        let read = tokio::time::timeout(BOUND, self.from_server.read_line(&mut line))
            .await
            .expect("reply within bound")
            .expect("read reply");
        assert!(read > 0, "server closed stdout before replying");
        serde_json::from_str(&line).expect("reply is JSON")
    }

    fn close_stdin(&mut self) {
        self.to_server = None;
    }
}

fn pipes() -> (Client, BufReader<DuplexStream>, DuplexStream) {
    let (client_in, server_in) = tokio::io::duplex(1 << 16);
    let (server_out, client_out) = tokio::io::duplex(1 << 16);
    (
        Client {
            to_server: Some(client_in),
            from_server: BufReader::new(client_out),
        },
        BufReader::new(server_in),
        server_out,
    )
}

async fn run_with<F, Fut>(
    server: &Server<FakeHost>,
    argv: &[&str],
    parent_exit: impl std::future::Future<Output = ()>,
    drive: F,
) -> (i32, String)
where
    F: FnOnce(Client) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    tokio::time::timeout(BOUND, run_unbounded(server, argv, parent_exit, drive))
        .await
        .expect("server finished within bound")
}

/// Like [`run_with`] without the overall bound, for tests that move a paused clock forward
/// (each reply is still awaited with a bounded timeout).
async fn run_unbounded<F, Fut>(
    server: &Server<FakeHost>,
    argv: &[&str],
    parent_exit: impl std::future::Future<Output = ()>,
    drive: F,
) -> (i32, String)
where
    F: FnOnce(Client) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let (client, stdin, stdout) = pipes();
    let mut stderr = Vec::new();
    let argv = args(argv);
    let (code, ()) = tokio::join!(
        run_cli(&argv, server, stdin, stdout, &mut stderr, parent_exit),
        drive(client)
    );
    (code, String::from_utf8(stderr).expect("utf8 stderr"))
}

// --- Test 1: CLI ---------------------------------------------------------------------------

#[test]
fn t1_argument_parsing() {
    assert_eq!(parse_args::<&str>(&[]), CliCommand::Serve);
    assert_eq!(parse_args(&["mcp"]), CliCommand::Serve);
    assert_eq!(parse_args(&["foo"]), CliCommand::Usage);
    assert_eq!(parse_args(&["mcp", "extra"]), CliCommand::Usage);
    assert_eq!(parse_args(&["--help"]), CliCommand::Usage);
}

#[tokio::test]
async fn t1_mcp_and_no_argument_serve() {
    for argv in [&[][..], &["mcp"][..]] {
        let server = server(FakeHost::windows_with_git_bash());
        let (code, stderr) = run_with(&server, argv, std::future::pending(), |mut client| async move {
            client
                .send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)
                .await;
            let reply = client.reply().await;
            assert_eq!(reply["id"], 1);
            assert_eq!(reply["result"]["serverInfo"]["name"], "git_bash");
            client.close_stdin();
        })
        .await;
        assert_eq!(code, 0, "{argv:?}");
        assert_eq!(stderr, "");
    }
}

#[tokio::test]
async fn t1_unknown_argument_prints_usage_and_exits_2() {
    let server = server(FakeHost::windows_with_git_bash());
    let (code, stderr) = run_with(&server, &["foo"], std::future::pending(), |mut client| async move {
        let mut rest = String::new();
        client
            .from_server
            .read_to_string(&mut rest)
            .await
            .expect("read stdout");
        assert_eq!(rest, "", "nothing on stdout");
    })
    .await;
    assert_eq!(code, 2);
    assert_eq!(stderr, format!("{USAGE}\n"));
    assert_eq!(stderr.lines().count(), 1);
}

#[tokio::test]
async fn t1_binary_rejects_unknown_argument() {
    let output = tokio::time::timeout(
        BOUND,
        tokio::process::Command::new(env!("CARGO_BIN_EXE_mhc-git-bash"))
            .arg("foo")
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    .expect("binary exits within bound")
    .expect("run binary");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(String::from_utf8_lossy(&output.stderr).lines().count(), 1);
}

// --- Test 2: non-Windows exits without serving ---------------------------------------------

#[tokio::test]
async fn t2_unavailable_server_exits_0_without_reading_or_writing() {
    let server = server(FakeHost::new("linux"));
    let (code, stderr) = run_with(&server, &["mcp"], std::future::pending(), |mut client| async move {
        // Stdin stays open: the server must not wait for it.
        let mut rest = String::new();
        client
            .from_server
            .read_to_string(&mut rest)
            .await
            .expect("read stdout");
        assert_eq!(rest, "");
    })
    .await;
    assert_eq!(code, 0);
    assert_eq!(stderr, "");
}

#[cfg(not(windows))]
#[tokio::test]
async fn t2_binary_exits_at_once_off_windows() {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_mhc-git-bash"))
        .arg("mcp")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn binary");
    // Keep stdin open so only an immediate exit can end the process.
    let _stdin = child.stdin.take();
    let mut stdout = child.stdout.take().expect("stdout");
    let mut stderr = child.stderr.take().expect("stderr");
    let status = tokio::time::timeout(BOUND, child.wait())
        .await
        .expect("exits within bound")
        .expect("wait");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    stdout.read_to_end(&mut out).await.expect("read stdout");
    stderr.read_to_end(&mut err).await.expect("read stderr");
    assert_eq!(status.code(), Some(0));
    assert!(out.is_empty());
    assert!(err.is_empty());
}

// --- Test 3: Windows + Git Bash ------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn t3_tool_order_idle_gap_and_eof() {
    let server = server(FakeHost::windows_with_git_bash());
    let (code, _) = run_unbounded(&server, &["mcp"], std::future::pending(), |mut client| async move {
        client
            .send(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
            .await;
        let reply = client.reply().await;
        let names: Vec<_> = reply["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|tool| tool["name"].clone())
            .collect();
        assert_eq!(names, [json!("run"), json!("which_bash"), json!("diagnose")]);

        // A day without requests: the (paused) clock jumps; nothing may end the server.
        tokio::time::advance(Duration::from_secs(24 * 60 * 60)).await;
        client
            .send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"diagnose"}}"#)
            .await;
        let reply = client.reply().await;
        assert_eq!(reply["id"], 2);
        assert_eq!(reply["result"]["isError"], false);
        let text = reply["result"]["content"][0]["text"].as_str().expect("text");
        let report: Value = serde_json::from_str(text).expect("diagnose JSON");
        assert_eq!(report["status"], "ready");
        assert_eq!(report["enabled"], true);
        assert_eq!(report["resolution"]["source"], "program-files");

        client.close_stdin();
        let mut rest = String::new();
        client
            .from_server
            .read_to_string(&mut rest)
            .await
            .expect("read until close");
        assert_eq!(rest, "");
    })
    .await;
    assert_eq!(code, 0);
}

#[tokio::test]
async fn t3_parent_exit_ends_serving() {
    let server = server(FakeHost::windows_with_git_bash());
    let (parent_gone, parent_exit) = tokio::sync::oneshot::channel::<()>();
    let parent_exit = async move {
        let _ = parent_exit.await;
    };
    let (code, _) = run_with(&server, &["mcp"], parent_exit, |mut client| async move {
        client
            .send(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
            .await;
        let _ = client.reply().await;
        parent_gone.send(()).expect("server still waiting");
        let mut rest = String::new();
        client
            .from_server
            .read_to_string(&mut rest)
            .await
            .expect("read until close");
        // Stdin is still open here; only the parent exit ended the server.
        assert!(client.to_server.is_some());
    })
    .await;
    assert_eq!(code, 0);
}

// --- Test 9: protocol ----------------------------------------------------------------------

async fn answer(line: &str) -> Option<Value> {
    handle_line(&server(FakeHost::windows_with_git_bash()), line).await
}

#[tokio::test]
async fn t9_initialize_echoes_or_defaults_protocol_version() {
    let reply = answer(
        r#"{"jsonrpc":"2.0","id":"a","method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
    )
    .await
    .expect("reply");
    assert_eq!(reply["id"], "a");
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(reply["result"]["capabilities"]["tools"]["listChanged"], false);
    assert_eq!(reply["result"]["serverInfo"]["name"], "git_bash");
    assert!(reply["result"]["serverInfo"]["version"].is_string());

    let reply = answer(r#"{"jsonrpc":"2.0","id":2,"method":"initialize"}"#)
        .await
        .expect("reply");
    assert_eq!(reply["result"]["protocolVersion"], "2024-11-05");
}

#[tokio::test]
async fn t9_error_codes() {
    let unknown = answer(r#"{"jsonrpc":"2.0","id":5,"method":"resources/list"}"#)
        .await
        .expect("reply");
    assert_eq!(unknown["id"], 5);
    assert_eq!(unknown["error"]["code"], -32601);

    let array = answer(r#"[{"jsonrpc":"2.0","id":6,"method":"tools/list"}]"#)
        .await
        .expect("reply");
    assert_eq!(array["id"], Value::Null);
    assert_eq!(array["error"]["code"], -32600);

    let garbage = answer("{not json").await.expect("reply");
    assert_eq!(garbage["id"], Value::Null);
    assert_eq!(garbage["error"]["code"], -32601);
}

#[tokio::test]
async fn t9_initialized_notification_has_no_reply() {
    assert_eq!(
        answer(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).await,
        None
    );
}

#[tokio::test]
async fn t9_garbage_line_then_next_request_is_answered_and_notifications_are_silent() {
    let server = server(FakeHost::windows_with_git_bash());
    let (code, _) = run_with(&server, &["mcp"], std::future::pending(), |mut client| async move {
        client.send("this is not json").await;
        let garbage = client.reply().await;
        assert_eq!(garbage["error"]["code"], -32601);
        assert_eq!(garbage["id"], Value::Null);

        client
            .send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await;
        client
            .send(r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"which_bash"}}"#)
            .await;
        // The very next line on stdout answers id 9: the notification produced nothing.
        let reply = client.reply().await;
        assert_eq!(reply["id"], 9);
        assert_eq!(reply["result"]["isError"], false);
        client.close_stdin();
    })
    .await;
    assert_eq!(code, 0);
}
