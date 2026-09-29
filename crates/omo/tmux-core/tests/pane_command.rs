//! Port of src/tmux-utils/pane-command.test.ts
//!
//! The TS "uses /bin/sh instead of inheriting SHELL" cases set `SHELL` first; the
//! Rust builders never read the environment, so those cases assert the same output
//! without mutating process-global state.

mod common;

use tmux_core::{build_tmux_attach_command, build_tmux_placeholder_command};

#[cfg(unix)]
fn run_with_fake_opencode(command: &str, temp_dir: &std::path::Path) -> Vec<String> {
    use std::os::unix::fs::PermissionsExt;

    let bin_dir = temp_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let opencode = bin_dir.join("opencode");
    std::fs::write(
        &opencode,
        "#!/bin/sh\nindex=0\nfor arg in \"$@\"; do\n  printf '%s\\t%s\\n' \"$index\" \"$arg\"\n  index=$((index + 1))\ndone",
    )
    .unwrap();
    std::fs::set_permissions(&opencode, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = std::process::Command::new("/bin/sh")
        .args(["-c", command])
        .env("PATH", path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .split('\n')
        .map(|line| line.split('\t').skip(1).collect::<Vec<_>>().join("\t"))
        .collect()
}

#[cfg(unix)]
fn cwd() -> String {
    std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

// describe("buildTmuxAttachCommand")

#[test]
fn attach_uses_bin_sh_instead_of_inheriting_shell() {
    let cmd = build_tmux_attach_command("http://localhost:3000", "ses_abc123", None);
    assert!(cmd.starts_with("/bin/sh -c \""));
    assert!(!cmd.contains("/bin/tcsh -c"));
}

#[cfg(unix)]
#[test]
fn attach_server_url_metacharacters_stay_one_literal_argument() {
    let temp = tempfile::Builder::new()
        .prefix("omo tmux command ")
        .tempdir()
        .unwrap();
    let server_url = "http://localhost:3000$(whoami);rm -rf /";
    let cmd = build_tmux_attach_command(server_url, "ses_abc123", None);
    assert_eq!(
        run_with_fake_opencode(&cmd, temp.path()),
        common::strings(&[
            "attach",
            server_url,
            "--session",
            "ses_abc123",
            "--dir",
            &cwd()
        ])
    );
}

#[test]
fn attach_escapes_session_id_shell_metacharacters() {
    let cmd = build_tmux_attach_command("http://localhost:3000", r#"ses_abc"$(whoami)""#, None);
    assert!(cmd.contains(r#"\""#));
    assert!(cmd.contains(r"\$"));
}

#[cfg(unix)]
#[test]
fn attach_directory_with_spaces_stays_one_argument() {
    let temp = tempfile::Builder::new()
        .prefix("omo tmux command ")
        .tempdir()
        .unwrap();
    let directory = temp.path().join("Mobile Documents").join("project");
    std::fs::create_dir_all(&directory).unwrap();
    let directory = directory.to_string_lossy().into_owned();
    let cmd = build_tmux_attach_command("http://localhost:3000", "ses_abc123", Some(&directory));
    assert_eq!(
        run_with_fake_opencode(&cmd, temp.path()),
        common::strings(&[
            "attach",
            "http://localhost:3000",
            "--session",
            "ses_abc123",
            "--dir",
            &directory
        ])
    );
}

#[test]
fn attach_places_server_url_before_session_and_dir() {
    let server_url = "http://127.0.0.1:4242";
    let cmd = build_tmux_attach_command(server_url, "ses_abc123", Some("/tmp/test-project"));
    let attach = cmd.find("opencode attach").unwrap();
    let url = cmd.find(server_url).unwrap();
    let session = cmd.find("--session").unwrap();
    let dir = cmd.find("--dir").unwrap();
    assert!(url > attach);
    assert!(session > url);
    assert!(dir > session);
}

#[test]
fn attach_places_session_id_after_session_flag() {
    let cmd =
        build_tmux_attach_command("http://127.0.0.1:3000", "ses_target123", Some("/tmp/proj"));
    let flag = cmd.find("--session").unwrap();
    assert!(cmd.find("ses_target123").unwrap() > flag);
}

#[test]
fn attach_falls_back_to_cwd_when_directory_omitted() {
    let cwd = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let cmd = build_tmux_attach_command("http://127.0.0.1:3000", "ses_abc123", None);
    assert!(cmd.contains(&cwd.replace('\\', "\\\\")));
    assert!(cmd.contains("--dir"));
}

// describe("buildTmuxPlaceholderCommand")

#[test]
fn placeholder_uses_bin_sh_instead_of_inheriting_shell() {
    let cmd = build_tmux_placeholder_command("My Task");
    assert!(cmd.starts_with("/bin/sh -c \""));
    assert!(!cmd.contains("/bin/csh -c"));
}

#[test]
fn placeholder_is_inert_instead_of_immediate_attach() {
    let cmd = build_tmux_placeholder_command("My Task");
    assert!(cmd.contains("Focus this pane to attach."));
    assert!(cmd.contains("while :; do sleep 86400; done"));
    assert!(!cmd.contains("opencode attach"));
}

#[test]
fn placeholder_keeps_single_quotes_and_percent_signs_inside_printf_arguments() {
    let cmd = build_tmux_placeholder_command("Fix Bob's 100% broken pane");
    assert!(cmd.contains(r"printf '%s\n%s\n'"));
    assert!(cmd.contains(r#"\"OMO subagent pane ready: Fix Bob's 100% broken pane\""#));
}
