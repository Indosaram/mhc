//! Drives the real `omo-git-bash` binary over stdio, the way the MCP client registers it.

use std::io::Write;
use std::process::Command;
use std::process::Stdio;

#[cfg(not(windows))]
#[test]
fn binary_stays_disabled_off_windows_and_writes_nothing() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_omo-git-bash"))
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn omo-git-bash");
    let mut stdin = child.stdin.take().expect("stdin");
    writeln!(
        stdin,
        "{{\"jsonrpc\":\"2.0\",\"id\":\"init\",\"method\":\"initialize\"}}"
    )
    .expect("write request");
    drop(stdin);

    let output = child.wait_with_output().expect("wait");

    assert!(output.status.success(), "exit status {:?}", output.status);
    assert_eq!(String::from_utf8(output.stdout).expect("utf8"), "");
}

#[test]
fn binary_reports_usage_for_an_unknown_command() {
    let output = Command::new(env!("CARGO_BIN_EXE_omo-git-bash"))
        .arg("bogus")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run omo-git-bash");

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(String::from_utf8(output.stdout).expect("utf8"), "");
    assert_eq!(
        String::from_utf8(output.stderr).expect("utf8"),
        "Usage: omo-git-bash [mcp]\n"
    );
}
