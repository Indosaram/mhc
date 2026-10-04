use maho_cli::cli::app_server::{
    daemon_verb, is_app_server_command, listening_banner, ShutdownSignals, SignalEscalation,
    USAGE_EXIT_CODE,
};
use maho_server::app_server::cli_args::{DaemonVerb, Listen};

#[test]
fn shutdown_escalates_from_graceful_to_forced() {
    let mut signals = ShutdownSignals::new();
    assert!(!signals.requested());
    assert!(!signals.forced());
    assert_eq!(signals.request(), SignalEscalation::First);
    assert!(signals.requested());
    assert_eq!(signals.request(), SignalEscalation::Forced);
    assert!(signals.forced());
    assert_eq!(signals.request(), SignalEscalation::Forced);
}

#[test]
fn only_the_app_server_verb_is_handled() {
    assert!(is_app_server_command(&["app-server".to_owned()]));
    assert!(is_app_server_command(&["app-server".to_owned(), "--listen".to_owned(), "stdio://".to_owned()]));
    assert!(!is_app_server_command(&["host".to_owned()]));
    assert!(!is_app_server_command(&[]));
}

#[test]
fn listening_banner_matches_the_pinned_transport_lines() {
    let stdio = Listen::Stdio { url: "stdio://".to_owned() };
    assert_eq!(listening_banner("maho code", &stdio), "maho code app-server listening on stdio://\n");

    let ws = Listen::Ws { url: "ws://127.0.0.1:18800".to_owned(), host: "127.0.0.1".to_owned(), port: 18800 };
    let banner = listening_banner("maho code", &ws);
    assert!(banner.contains("listening on ws://127.0.0.1:18800"), "{banner}");
    assert!(banner.contains("readyz http://127.0.0.1:18800/readyz"), "{banner}");

    let unix = Listen::Unix { url: "unix:///tmp/app.sock".to_owned(), path: Some("/tmp/app.sock".to_owned()) };
    assert_eq!(listening_banner("maho code", &unix), "maho code app-server listening on unix:///tmp/app.sock\n");
}

#[test]
fn daemon_verbs_are_recognized_from_the_argument_vector() {
    let args = vec!["app-server".to_owned(), "daemon".to_owned(), "status".to_owned()];
    assert_eq!(daemon_verb(&args), Some(DaemonVerb::Status));
    let restart = vec!["app-server".to_owned(), "daemon".to_owned(), "restart".to_owned()];
    assert_eq!(daemon_verb(&restart), Some(DaemonVerb::Restart));
    let server = vec!["app-server".to_owned(), "--listen".to_owned(), "stdio://".to_owned()];
    assert_eq!(daemon_verb(&server), None);
}

#[test]
fn real_entry_reports_a_usage_error_with_exit_code_two() {
    let dir = tempfile::tempdir().expect("isolated CLI directory");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_mhc"))
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env_remove("__PI_INTERNAL_SPAWN")
        .args(["app-server", "--listen", "bogus://"])
        .output()
        .expect("run real CLI");
    assert_eq!(output.status.code(), Some(USAGE_EXIT_CODE));
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("Invalid --listen value"), "{stderr}");
    assert!(stderr.contains("Usage:"), "{stderr}");
}

#[test]
fn real_entry_rejects_an_unknown_app_server_option() {
    let dir = tempfile::tempdir().expect("isolated CLI directory");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_mhc"))
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env_remove("__PI_INTERNAL_SPAWN")
        .args(["app-server", "--nope"])
        .output()
        .expect("run real CLI");
    assert_eq!(output.status.code(), Some(USAGE_EXIT_CODE));
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("Unexpected app-server argument: --nope"), "{stderr}");
}
