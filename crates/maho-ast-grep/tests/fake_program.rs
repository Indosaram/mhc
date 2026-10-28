//! Acceptance tests 11, 16, 17, 18, 19 and 24 of `docs/cleanroom/specs/ast-grep.md` §8 with fake
//! `ast-grep` programs (shell scripts), so exact outputs, exit codes and hangs can be produced.
#![cfg(unix)]

mod common;

use common::{call, code_of, fake, only, record, server_with, tree};
use maho_ast_grep::{abort_channel, handle_message, serve};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const BOUND: Duration = Duration::from_secs(30);

fn search_args(dir: &Path) -> Value {
    json!({ "paths": ["."], "pattern": "x()", "language": "javascript", "workdir": dir })
}

/// A fake program that prints the contents of `records.txt` (kept beside it) and then `tail`.
fn printing(dir: &Path, records: &[String], tail: &str, exit: i32) -> maho_ast_grep::Server {
    std::fs::write(dir.join("records.txt"), records.join("\n") + "\n").expect("write records");
    let program = fake::program(
        dir,
        "fake-sg",
        &format!("cat '{}/records.txt'\n{tail}\nexit {exit}", dir.display()),
    );
    server_with(only(&program), Some(dir.to_path_buf()))
}

#[tokio::test]
async fn t11_file_outside_workdir_keeps_its_absolute_path() {
    let work = tempfile::tempdir().expect("tempdir");
    let other = tempfile::tempdir().expect("tempdir");
    tree(work.path(), &[("in.js", "")]);
    let outside = other.path().join("out.js");
    let inside = work.path().join("in.js");
    let server = printing(
        work.path(),
        &[
            record(&outside.to_string_lossy(), 10),
            record(&inside.to_string_lossy(), 0),
        ],
        "",
        0,
    );
    let (value, is_error) = call(&server, "search", search_args(work.path())).await;
    assert!(!is_error, "{value}");
    let paths: Vec<&str> = value["matches"]
        .as_array()
        .expect("matches")
        .iter()
        .filter_map(|m| m["path"].as_str())
        .collect();
    let absolute = outside.to_string_lossy().replace('\\', "/");
    assert!(paths.contains(&"in.js"), "{paths:?}");
    assert!(paths.contains(&absolute.as_str()), "{paths:?}");
}

#[tokio::test]
async fn t16_garbage_after_a_record_is_salvaged_garbage_alone_fails_and_long_lines_fail() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = printing(dir.path(), &[record("a.js", 0)], "echo 'not json at all'", 0);
    let (value, is_error) = call(&server, "search", search_args(dir.path())).await;
    assert!(!is_error, "{value}");
    assert_eq!(value["matches"].as_array().map(Vec::len), Some(1));
    assert_eq!(value["truncation"]["reason"], "sg_output_truncated");
    assert_eq!(value["truncation"]["salvagedRecords"], 1);

    let only_garbage = tempfile::tempdir().expect("tempdir");
    let server = printing(only_garbage.path(), &["garbage".to_owned()], "", 0);
    let (value, is_error) = call(&server, "search", search_args(only_garbage.path())).await;
    assert!(is_error);
    assert_eq!(code_of(&value), "OUTPUT_PARSE_FAILED");
    assert_eq!(value["error"]["retryable"], true);

    let long = tempfile::tempdir().expect("tempdir");
    let server = printing(
        long.path(),
        &[],
        "head -c 1100000 /dev/zero | tr '\\000' 'a'; echo",
        0,
    );
    let (value, is_error) = call(&server, "search", search_args(long.path())).await;
    assert!(is_error);
    assert_eq!(code_of(&value), "OUTPUT_TOO_LARGE");
}

#[tokio::test]
async fn t17_exit_codes_decide_between_failure_and_no_matches() {
    let two = tempfile::tempdir().expect("tempdir");
    let server = printing(two.path(), &[], "", 2);
    let (value, is_error) = call(&server, "search", search_args(two.path())).await;
    assert!(is_error);
    assert_eq!(code_of(&value), "SG_FAILED");
    assert_eq!(value["error"]["retryable"], false);

    let flagged = tempfile::tempdir().expect("tempdir");
    let server = printing(flagged.path(), &[], "echo 'error: boom' >&2", 1);
    let (value, _) = call(&server, "search", search_args(flagged.path())).await;
    assert_eq!(code_of(&value), "SG_FAILED");

    let quiet = tempfile::tempdir().expect("tempdir");
    let server = printing(quiet.path(), &[], "", 1);
    let (value, is_error) = call(&server, "search", search_args(quiet.path())).await;
    assert!(!is_error, "{value}");
    assert_eq!(value["ok"], true);
    assert_eq!(value["counts"]["returnedMatches"], 0);
    assert_eq!(value["counts"]["totalMatches"], 0);
}

fn process_is_gone(pid: u32) -> bool {
    !std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[tokio::test]
async fn t18_a_hanging_program_times_out_and_the_child_is_gone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pid_file = dir.path().join("pid");
    let program = fake::program(
        dir.path(),
        "hang-sg",
        &format!("echo $$ > '{}'\nexec sleep 600", pid_file.display()),
    );
    let server = server_with(only(&program), Some(dir.path().to_path_buf()));
    let mut args = search_args(dir.path());
    args["timeoutMs"] = json!(1000);
    let (value, is_error) = tokio::time::timeout(BOUND, call(&server, "search", args))
        .await
        .expect("returns within bound");
    assert!(is_error);
    assert_eq!(code_of(&value), "TIMEOUT");
    assert_eq!(value["error"]["retryable"], true);
    let pid: u32 = std::fs::read_to_string(&pid_file)
        .expect("pid file")
        .trim()
        .parse()
        .expect("pid");
    assert!(process_is_gone(pid), "child {pid} is still running");
}

#[tokio::test]
async fn t19_parent_exit_aborts_the_running_call_and_the_next_call_has_a_fresh_signal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fifo = dir.path().join("started");
    let made = std::process::Command::new("mkfifo").arg(&fifo).status().expect("mkfifo");
    assert!(made.success());
    let program = fake::program(
        dir.path(),
        "hang-sg",
        &format!("echo started > '{}'\nexec sleep 600", fifo.display()),
    );
    let server = server_with(only(&program), Some(dir.path().to_path_buf()));

    let (client_in, server_in) = tokio::io::duplex(1 << 16);
    let (server_out, client_out) = tokio::io::duplex(1 << 16);
    let (parent_tx, parent_rx) = tokio::sync::oneshot::channel::<()>();
    let serving = tokio::spawn(async move {
        serve(&server, BufReader::new(server_in), server_out, async {
            let _ = parent_rx.await;
        })
        .await
    });

    let mut client_in = client_in;
    let request = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "search", "arguments": search_args(dir.path()) } });
    client_in
        .write_all(format!("{request}\n").as_bytes())
        .await
        .expect("send request");
    // The child announces itself through the fifo: the call is provably in progress.
    let fifo_path = fifo.clone();
    tokio::time::timeout(
        BOUND,
        tokio::task::spawn_blocking(move || std::fs::read_to_string(fifo_path)),
    )
    .await
    .expect("child started")
    .expect("join")
    .expect("read fifo");
    parent_tx.send(()).expect("parent exit");

    let mut lines = BufReader::new(client_out).lines();
    let line = tokio::time::timeout(BOUND, lines.next_line())
        .await
        .expect("reply within bound")
        .expect("read")
        .expect("a reply line");
    let reply: Value = serde_json::from_str(&line).expect("json");
    let text = reply["result"]["content"][0]["text"].as_str().expect("text");
    let value: Value = serde_json::from_str(text).expect("result json");
    assert_eq!(code_of(&value), "ABORTED");
    assert_eq!(value["error"]["retryable"], true);
    tokio::time::timeout(BOUND, serving)
        .await
        .expect("server stops")
        .expect("join")
        .expect("serve result");

    // A later call has its own, unaborted signal.
    let quick = fake::program(dir.path(), "quick-sg", "exit 1");
    let server = server_with(only(&quick), Some(dir.path().to_path_buf()));
    let (handle, first) = abort_channel();
    handle.abort();
    assert!(first.is_aborted());
    let (_fresh_handle, mut fresh) = abort_channel();
    assert!(!fresh.is_aborted());
    let request = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": { "name": "search", "arguments": search_args(dir.path()) } });
    let reply = handle_message(&server, &request.to_string(), &mut fresh).await.expect("reply");
    let text = reply["result"]["content"][0]["text"].as_str().expect("text");
    let value: Value = serde_json::from_str(text).expect("json");
    assert_eq!(value["ok"], true, "{value}");
}

#[tokio::test]
async fn t24_a_second_pass_that_finds_nothing_is_a_stale_preview() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("records.txt"), record("a.js", 0) + "\n").expect("records");
    let program = fake::program(
        dir.path(),
        "stale-sg",
        &format!(
            "for a in \"$@\"; do if [ \"$a\" = \"--update-all\" ]; then exit 1; fi; done\ncat '{}/records.txt'",
            dir.path().display()
        ),
    );
    let server = server_with(only(&program), Some(dir.path().to_path_buf()));
    let (value, is_error) = call(
        &server,
        "rewrite",
        json!({ "paths": ["."], "pattern": "x()", "rewrite": "y()", "language": "javascript",
                "apply": true, "workdir": dir.path() }),
    )
    .await;
    assert!(is_error, "{value}");
    assert_eq!(code_of(&value), "REWRITE_STALE_PREVIEW");
    assert_eq!(value["error"]["retryable"], true);
    assert_eq!(value["error"]["phase"], "apply");
}
