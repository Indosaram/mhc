//! Port of src/tmux-utils/pane-password-channel.test.ts

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::common::{Recorder, enabled_config, env, healthy_deps, success};
use tmux_core::{
    PaneAuthFallbackDirectory, SpawnTmuxPaneRequest, SplitDirection, TmuxDeps,
    build_pane_auth_environment_args_from, build_tmux_attach_command_from,
    create_pane_auth_directory, spawn_tmux_pane,
};

#[test]
fn pane_argv_carries_only_a_private_one_use_file_and_the_pane_consumes_the_password_without_argv_exposure()
 {
    let password = ["pane", "'", "$()", "\n", "credential", "\n"].concat();
    let sandbox = tempfile::Builder::new()
        .prefix("pane-auth-")
        .tempdir()
        .expect("sandbox");
    let directory = create_pane_auth_directory().expect("pane auth directory");
    let directory_text = directory.path().to_string_lossy().into_owned();
    let sandbox_text = sandbox.path().to_string_lossy().into_owned();
    let environment = env(&[
        ("OPENCODE_SERVER_PASSWORD", password.as_str()),
        ("OPENCODE_SERVER_PASSWORD_DIR", directory_text.as_str()),
    ]);

    let args =
        build_pane_auth_environment_args_from(&*environment, &PaneAuthFallbackDirectory::default())
            .expect("auth args");
    let command = build_tmux_attach_command_from(
        "http://127.0.0.1:4096",
        "ses-test",
        Some(&sandbox_text),
        &*environment,
    );

    assert!(
        !args
            .iter()
            .chain(std::iter::once(&command))
            .any(|arg| arg.contains(&password))
    );
    let password_file = args
        .iter()
        .find_map(|arg| arg.strip_prefix("OPENCODE_SERVER_PASSWORD_FILE="))
        .expect("one-use password file")
        .to_owned();
    let mode = |path: &Path| std::fs::metadata(path).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode(Path::new(&password_file)), 0o600);
    assert_eq!(
        mode(Path::new(&password_file).parent().expect("parent")),
        0o700
    );
    let opencode = sandbox.path().join("opencode");
    std::fs::write(
        &opencode,
        [
            "#!/bin/sh",
            r#"test "$OPENCODE_SERVER_PASSWORD" = "$EXPECTED_PASSWORD" || exit 2"#,
            r#"test ! -e "$EXPECTED_FILE" || exit 3"#,
            r#"for arg in "$@"; do test "$arg" != "$EXPECTED_PASSWORD" || exit 4; done"#,
            "printf auth-ok",
        ]
        .join("\n"),
    )
    .expect("fake opencode");
    std::fs::set_permissions(&opencode, std::fs::Permissions::from_mode(0o700))
        .expect("fake opencode mode");

    let output = std::process::Command::new("/bin/sh")
        .args(["-c", &command])
        .env(
            "PATH",
            format!(
                "{}:{}",
                sandbox.path().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("OPENCODE_SERVER_PASSWORD", "")
        .env("OPENCODE_SERVER_PASSWORD_FILE", &password_file)
        .env("EXPECTED_PASSWORD", &password)
        .env("EXPECTED_FILE", &password_file)
        .output()
        .expect("run attach command");

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "auth-ok");
    assert!(!Path::new(&password_file).exists());
}

fn password_file_arg(args: &[String]) -> String {
    args.iter()
        .find_map(|arg| arg.strip_prefix("OPENCODE_SERVER_PASSWORD_FILE="))
        .expect("one-use password file")
        .to_owned()
}

#[test]
fn dropping_the_fallback_directory_removes_password_files_no_pane_consumed() {
    let fallback = PaneAuthFallbackDirectory::default();
    let environment = env(&[("OPENCODE_SERVER_PASSWORD", "unconsumed")]);

    let first = password_file_arg(
        &build_pane_auth_environment_args_from(&*environment, &fallback).expect("auth args"),
    );
    let second = password_file_arg(
        &build_pane_auth_environment_args_from(&*environment, &fallback).expect("auth args"),
    );
    let directory = Path::new(&first).parent().expect("parent").to_path_buf();
    assert_eq!(Path::new(&second).parent(), Some(directory.as_path()));
    assert!(Path::new(&first).exists() && Path::new(&second).exists());

    drop(fallback);

    assert!(!Path::new(&first).exists());
    assert!(!Path::new(&second).exists());
    assert!(!directory.exists());
}

#[test]
fn a_pane_spawned_without_a_password_dir_leaves_no_password_file_once_its_deps_are_dropped() {
    let recorder = Recorder::new(vec![success("%pane"), success("")]);
    let deps = TmuxDeps {
        environment: env(&[
            ("TMUX", "/tmp/tmux-1000/default,1234,0"),
            ("OPENCODE_SERVER_PASSWORD", "unconsumed"),
        ]),
        ..healthy_deps(&recorder, "tmux")
    };
    let config = enabled_config();

    spawn_tmux_pane(
        &SpawnTmuxPaneRequest {
            session_id: "session-1",
            description: "worker",
            config: &config,
            server_url: "http://127.0.0.1:4321",
            directory: "/tmp/project",
            target_pane_id: None,
            split_direction: SplitDirection::Horizontal,
        },
        &deps,
    );
    let password_file = password_file_arg(&recorder.call(0).1);
    assert!(Path::new(&password_file).exists());

    drop(deps);

    assert!(!Path::new(&password_file).exists());
    assert!(!Path::new(&password_file).parent().expect("parent").exists());
}
