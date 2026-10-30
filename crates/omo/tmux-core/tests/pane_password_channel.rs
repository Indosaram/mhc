//! Port of src/tmux-utils/pane-password-channel.test.ts

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::common::env;
use tmux_core::{
    build_pane_auth_environment_args_from, build_tmux_attach_command_from,
    create_pane_auth_directory,
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

    let args = build_pane_auth_environment_args_from(&*environment).expect("auth args");
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
