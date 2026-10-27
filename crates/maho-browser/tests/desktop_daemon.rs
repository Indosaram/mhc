//! The vendored desktop daemon, run for real with the local Python. Without the `cua` package it
//! still starts, reports the import error in its ready event, answers `ping`, and turns screen
//! actions into a clear error.
use maho_browser::computer::{
    ComputerBackend, DesktopDaemon, DesktopTarget, Point, install_daemon_script,
};
use std::time::Duration;

fn python() -> Option<&'static str> {
    let ok = std::process::Command::new("python3")
        .arg("-c")
        .arg("import sys; sys.exit(0 if sys.version_info >= (3, 8) else 1)")
        .status()
        .ok()?
        .success();
    ok.then_some("python3")
}

#[test]
fn daemon_starts_answers_and_reports_missing_cua() {
    let Some(python) = python() else {
        eprintln!("skipped: python3 unavailable");
        return;
    };
    let Ok(dir) = tempfile::tempdir() else { return };
    let script = install_daemon_script(dir.path()).unwrap_or_else(|error| panic!("{error}"));
    // Installing again reuses the same file.
    assert_eq!(install_daemon_script(dir.path()).ok(), Some(script.clone()));
    let mut daemon = DesktopDaemon::start(python, &script, Duration::from_secs(20))
        .unwrap_or_else(|error| panic!("daemon did not start: {error}"));
    assert_eq!(daemon.ready.version, "0.1.0");
    assert_eq!(daemon.ping().ok().as_deref(), Some("0.1.0"));
    assert_eq!(daemon.target, DesktopTarget::Localhost);
    assert_eq!(daemon.label(), "desktop");
    assert_eq!(daemon.list_sandboxes().map(|list| list.len()), Ok(0));
    if !daemon.ready.cua_available {
        assert!(daemon.ready.cua_import_error.is_some());
        let error = daemon.screenshot().err().unwrap_or_default();
        assert!(error.contains("cua Python package is not installed"), "{error}");
        let error = daemon
            .click(Point { x: 1.0, y: 2.0 }, "left", 1, &[])
            .err()
            .unwrap_or_default();
        assert!(error.contains("-32001"), "{error}");
    }
    let unknown = daemon.call("teleport", serde_json::json!({}), None).err().unwrap_or_default();
    assert!(unknown.contains("Unknown method"), "{unknown}");
    assert!(daemon.move_to(Point { x: 0.0, y: 0.0 }).is_err());
    daemon.shutdown();
    assert!(daemon.ping().is_err());
}

#[test]
fn a_missing_interpreter_is_reported() {
    let Ok(dir) = tempfile::tempdir() else { return };
    let Ok(script) = install_daemon_script(dir.path()) else { return };
    let error = DesktopDaemon::start("/nonexistent/python3", &script, Duration::from_secs(5))
        .err()
        .unwrap_or_default();
    assert!(error.contains("cannot start the desktop daemon"), "{error}");
}
