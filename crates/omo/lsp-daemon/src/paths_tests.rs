use super::*;
use pretty_assertions::assert_eq;

const DEFAULT_CLI: &str = "/packaged/cli.js";

fn default_runtime() -> DaemonRuntimeDefaults {
    DaemonRuntime {
        cli_path: PathBuf::from(DEFAULT_CLI),
        version: "1.2.3".to_string(),
    }
}

fn unix_platform(home: &str) -> DaemonPlatform {
    DaemonPlatform {
        flavor: PathFlavor::Posix,
        home_dir: home.to_string(),
        tmp_dir: "/tmp".to_string(),
        uid: Some(501),
        username: "qa-user".to_string(),
    }
}

fn windows_platform(username: &str) -> DaemonPlatform {
    DaemonPlatform {
        flavor: PathFlavor::Win32,
        home_dir: "C:\\Users\\qa".to_string(),
        tmp_dir: "C:\\Temp".to_string(),
        uid: None,
        username: username.to_string(),
    }
}

fn env(pairs: &[(&str, &str)]) -> Env {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

#[test]
fn public_environment_names_are_exactly_the_three_omo_variables() {
    let mut names = vec![
        OMO_LSP_DAEMON_DIR,
        OMO_LSP_DAEMON_CLI,
        OMO_LSP_DAEMON_VERSION,
    ];
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "MAHO_LSP_DAEMON_DIR",
            "OMO_LSP_DAEMON_CLI",
            "OMO_LSP_DAEMON_VERSION"
        ]
    );
}

#[test]
fn default_base_is_influenced_only_by_the_user_home() {
    let env = env(&[
        ("CODEX_HOME", "/ignored/codex"),
        ("PLUGIN_DATA", "/ignored/plugin"),
        ("CODEX_LSP_DAEMON_DIR", "/ignored/legacy"),
    ]);
    assert_eq!(
        daemon_base_dir(&env, &unix_platform("/Users/qa")),
        Ok("/Users/qa/.maho/lsp-daemon".to_string())
    );
}

#[test]
fn absolute_directory_override_is_normalized() {
    let env = env(&[(OMO_LSP_DAEMON_DIR, "/tmp/omo/../shared")]);
    assert_eq!(
        daemon_base_dir(&env, &unix_platform("/Users/qa")),
        Ok("/tmp/shared".to_string())
    );
}

#[test]
fn relative_directory_override_is_rejected() {
    let env = env(&[(OMO_LSP_DAEMON_DIR, "relative/state")]);
    assert_eq!(
        daemon_base_dir(&env, &unix_platform("/Users/qa")),
        Err(InvalidDaemonDirectoryError {
            directory: "relative/state".to_string()
        })
    );
}

#[test]
fn version_grammar_accepts_and_rejects_the_ts_table() {
    for valid in ["0", "1.2.3", "release_2026-07+build.5", "A"] {
        assert_eq!(validate_daemon_version(valid), Ok(valid.to_string()));
    }
    let too_long = "a".repeat(129);
    for invalid in [
        "",
        " 1.2.3",
        "1.2.3 ",
        "../1",
        ".hidden",
        "a/b",
        "a\\b",
        too_long.as_str(),
    ] {
        assert!(validate_daemon_version(invalid).is_err(), "{invalid:?}");
    }
}

#[test]
fn every_artifact_is_isolated_under_the_version_dir() {
    let env = env(&[(OMO_LSP_DAEMON_DIR, "/var/omo")]);
    let paths =
        daemon_paths_with(&env, &default_runtime(), &unix_platform("/Users/qa")).expect("paths");
    assert_eq!(
        paths,
        DaemonPaths {
            cli_path: PathBuf::from(DEFAULT_CLI),
            ..DaemonPaths::under_dir("/var/omo/v1.2.3", "1.2.3")
        }
    );
}

#[test]
fn long_unix_version_dir_uses_the_short_hash_fallback() {
    let base = format!("/{}", "x".repeat(140));
    let env = env(&[(OMO_LSP_DAEMON_DIR, base.as_str())]);
    let paths =
        daemon_paths_with(&env, &default_runtime(), &unix_platform("/Users/qa")).expect("paths");
    assert_eq!(
        paths.socket,
        PathBuf::from("/tmp/omo-lsp-1.2.3-b174cf2e082d87e3/daemon.sock")
    );
    assert!(paths.socket.as_os_str().len() < 100);
}

#[test]
fn windows_pipe_digest_binds_version_dir_and_user() {
    let env = env(&[(OMO_LSP_DAEMON_DIR, "C:\\omo\\daemon")]);
    let first =
        daemon_paths_with(&env, &default_runtime(), &windows_platform("qa-user")).expect("first");
    let second = daemon_paths_with(&env, &default_runtime(), &windows_platform("other-user"))
        .expect("second");
    assert_eq!(
        first.socket,
        PathBuf::from("\\\\.\\pipe\\omo-lsp-1.2.3-62c673605928ff0f")
    );
    assert_ne!(second.socket, first.socket);
}

#[test]
fn neither_override_preserves_packaged_runtime() {
    assert_eq!(
        resolve_daemon_runtime(&Env::new(), &default_runtime()),
        Ok(default_runtime())
    );
}

#[test]
fn both_overrides_preserve_the_explicit_pair() {
    let root = tempfile::tempdir().expect("tempdir");
    let cli = root.path().join("cli.js");
    std::fs::write(&cli, "#!/usr/bin/env node\n").expect("cli");
    let cli_text = cli.to_string_lossy().into_owned();
    let env = env(&[
        (OMO_LSP_DAEMON_CLI, cli_text.as_str()),
        (OMO_LSP_DAEMON_VERSION, "9.8.7+qa"),
    ]);
    assert_eq!(
        resolve_daemon_runtime(&env, &default_runtime()),
        Ok(DaemonRuntime {
            cli_path: cli,
            version: "9.8.7+qa".to_string()
        })
    );
}

#[test]
fn singleton_and_invalid_cli_overrides_are_rejected() {
    let root = tempfile::tempdir().expect("tempdir");
    let directory = root.path().join("cli.js");
    std::fs::create_dir(&directory).expect("dir");
    let directory = directory.to_string_lossy().into_owned();
    let missing = root.path().join("missing-omo-lsp-cli.js");
    let missing = missing.to_string_lossy().into_owned();
    let cases = [
        env(&[(OMO_LSP_DAEMON_CLI, "/tmp/cli.js")]),
        env(&[(OMO_LSP_DAEMON_VERSION, "9.8.7")]),
        env(&[
            (OMO_LSP_DAEMON_CLI, "relative/cli.js"),
            (OMO_LSP_DAEMON_VERSION, "1.2.3"),
        ]),
        env(&[
            (OMO_LSP_DAEMON_CLI, missing.as_str()),
            (OMO_LSP_DAEMON_VERSION, "1.2.3"),
        ]),
        env(&[
            (OMO_LSP_DAEMON_CLI, directory.as_str()),
            (OMO_LSP_DAEMON_VERSION, "1.2.3"),
        ]),
    ];
    for case in cases {
        let error = resolve_daemon_runtime(&case, &default_runtime()).expect_err("rejected");
        assert_eq!(error.code(), "invalid_runtime_override");
    }
}

#[test]
fn pair_validation_wins_before_directory_lookup() {
    let root = tempfile::tempdir().expect("tempdir");
    let cli = root.path().join("cli.js");
    std::fs::write(&cli, "x").expect("cli");
    let cli = cli.to_string_lossy().into_owned();
    let env = env(&[
        (OMO_LSP_DAEMON_CLI, cli.as_str()),
        (OMO_LSP_DAEMON_DIR, "relative/state"),
    ]);
    let result = daemon_paths_with(&env, &default_runtime(), &unix_platform("/Users/qa"));
    assert!(
        matches!(result, Err(DaemonPathsError::Runtime(_))),
        "{result:?}"
    );
}

#[test]
fn stamped_version_is_the_crate_version() {
    assert_eq!(resolve_daemon_version(), env!("CARGO_PKG_VERSION"));
}
