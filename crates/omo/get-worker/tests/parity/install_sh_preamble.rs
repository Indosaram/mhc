//! Port of `test/install-sh-preamble.test.ts`: the POSIX wrapper around the Bash body.
//!
//! These are real subprocess tests: `/bin/sh` and `/bin/bash` read the shipped
//! `install.sh` bytes, exactly as the upstream suite does. Each test owns a temp dir
//! that is removed on drop.

#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use get_worker::INSTALL_SH;

const BASH_REQUIRED: &str =
    "omo installer: bash is required; run with: curl -fsSL https://get.omo.dev/install.sh | bash\n";

fn run_shell(shell: &str, args: &[&str], path: &Path, tmp: &Path) -> Output {
    let mut child = Command::new(shell)
        .args(args)
        .env("HOME", tmp.join("home"))
        .env("PATH", path)
        .env("TMPDIR", tmp)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the shell");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(INSTALL_SH.as_bytes())
        .expect("write the installer to stdin");
    child.wait_with_output().expect("wait for the shell")
}

fn run_bash_syntax_check(script: &str) -> Output {
    let mut child = Command::new("/bin/bash")
        .arg("-n")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bash");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(script.as_bytes())
        .expect("write the script");
    child.wait_with_output().expect("wait for bash")
}

fn empty_bin(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    fs::create_dir(&bin).expect("bin dir");
    bin
}

fn link_tool(bin: &Path, name: &str) {
    let source = ["/bin", "/usr/bin"]
        .iter()
        .map(|dir| Path::new(dir).join(name))
        .find(|candidate| candidate.exists())
        .unwrap_or_else(|| panic!("missing test command: {name}"));
    std::os::unix::fs::symlink(source, bin.join(name)).expect("symlink the tool");
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn bash_program() -> &'static str {
    let open = "<<'OMO_INSTALL_BASH'\n";
    let start = INSTALL_SH.find(open).expect("the heredoc opener") + open.len();
    let rest = &INSTALL_SH[start..];
    let end = rest.find("\nOMO_INSTALL_BASH\n").expect("the heredoc closer");
    &rest[..end]
}

fn assert_bash_hint(output: &Output) {
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(&*stderr, BASH_REQUIRED);
    assert!(!stderr.contains("syntax error"), "{stderr}");
}

#[test]
fn sh_prints_one_bash_hint_and_never_parses_the_bash_body_when_bash_is_unavailable() {
    let root = tempfile::tempdir().expect("temp dir");
    let bin = empty_bin(root.path());
    let output = run_shell("/bin/sh", &[], &bin, root.path());
    assert_bash_hint(&output);
}

#[test]
fn dash_prints_one_bash_hint_and_never_parses_the_bash_body_when_bash_is_unavailable() {
    if !Path::new("/bin/dash").exists() {
        println!("dash is not installed; the upstream loop filters this shell out too");
        return;
    }
    let root = tempfile::tempdir().expect("temp dir");
    let bin = empty_bin(root.path());
    let output = run_shell("/bin/dash", &[], &bin, root.path());
    assert_bash_hint(&output);
}

#[test]
fn sh_hands_every_unread_line_and_every_argument_to_bash() {
    let root = tempfile::tempdir().expect("temp dir");
    let bin = empty_bin(root.path());
    for tool in ["mktemp", "rm", "cat", "cp"] {
        link_tool(&bin, tool);
    }
    let captured = root.path().join("captured.sh");
    let args_file = root.path().join("args");
    let fake_bash = bin.join("bash");
    fs::write(
        &fake_bash,
        format!(
            "#!/bin/sh\ncp \"$1\" {captured}\nshift\nprintf '%s\\n' \"$@\" >{args}\n",
            captured = shell_quote(&captured),
            args = shell_quote(&args_file),
        ),
    )
    .expect("write the fake bash");
    fs::set_permissions(&fake_bash, fs::Permissions::from_mode(0o755)).expect("chmod the fake bash");

    let output = run_shell("/bin/sh", &["-s", "alpha", "beta"], &bin, root.path());
    assert_eq!(output.status.code(), Some(0));
    let captured_text = fs::read_to_string(&captured).expect("the captured body");
    assert!(captured_text.starts_with("#!/usr/bin/env bash\n# OmO native installer"));
    assert!(captured_text.ends_with("if [[ \"${BASH_SOURCE[0]}\" == \"$0\" ]]; then main \"$@\"; fi\n"));
    assert_eq!(fs::read_to_string(&args_file).expect("the captured args"), "alpha\nbeta\n");
}

#[test]
fn bash_still_parses_the_wrapper_and_the_complete_installer_body() {
    let wrapper = run_bash_syntax_check(INSTALL_SH);
    let body = bash_program();
    let installer = run_bash_syntax_check(body);
    assert_eq!(
        wrapper.status.code(),
        Some(0),
        "wrapper: {}",
        String::from_utf8_lossy(&wrapper.stderr)
    );
    assert_eq!(
        installer.status.code(),
        Some(0),
        "installer: {}",
        String::from_utf8_lossy(&installer.stderr)
    );
    let stderr = format!(
        "{}{}",
        String::from_utf8_lossy(&wrapper.stderr),
        String::from_utf8_lossy(&installer.stderr)
    );
    assert_eq!(stderr, "");
}
