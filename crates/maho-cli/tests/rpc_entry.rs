//! senpi `rpc-entry.ts`: the `<app>-rpc` entry is `main(["--mode", "rpc", ...argv])`. The native
//! binary answers that entry when it is invoked through an argv0 whose stem ends in `-rpc`.

#[cfg(unix)]
#[test]
fn an_rpc_named_entry_runs_the_rpc_mode_without_a_mode_flag() {
    use std::{
        fs,
        io::Write,
        process::{Command, Stdio},
    };
    let home = tempfile::tempdir().expect("isolated home");
    fs::create_dir_all(home.path().join(".omo")).expect("isolated omo state");
    fs::write(home.path().join(".omo/onboarding-completed"), "{}\n")
        .expect("onboarding already complete");
    let entry = home.path().join("mhc-rpc");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_mhc"), &entry).expect("rpc entry link");
    let mut child = Command::new(&entry)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home.path())
        .current_dir(home.path())
        .args(["--offline", "--no-session", "--model", "openai/gpt-4o"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("rpc entry");
    let mut stdin = child.stdin.take().expect("stdin");
    stdin
        .write_all(b"{\"id\":\"rpc-entry-probe\",\"type\":\"get_messages\"}\n")
        .expect("query");
    drop(stdin);
    let output = child.wait_with_output().expect("process exits");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("RPC UTF-8");
    let response = stdout
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("RPC JSONL frame"))
        .find(|frame| frame["id"] == "rpc-entry-probe")
        .expect("the -rpc entry answered on the RPC protocol");
    assert_eq!(response["success"], true);
}
