//! Port of transport-sdk.ts `pipeStderr` (v2026.10.27-2): stdio stderr is held until the era is classified,
//! discarded for a modern child, released for a legacy one, and redacted under private forms.
use maho_ext_mcp::{
    log::McpLogger, modern::request::ModernProtocolState, transport_sdk::StderrDiagnostics,
};
use std::sync::{Arc, Mutex, atomic::Ordering};

fn setup() -> (
    tempfile::TempDir,
    Arc<Mutex<McpLogger>>,
    Arc<StderrDiagnostics>,
    ModernProtocolState,
) {
    let root = tempfile::tempdir().unwrap();
    let logger = Arc::new(Mutex::new(
        McpLogger::new("stderr", root.path(), None).unwrap(),
    ));
    let diagnostics = StderrDiagnostics::new(logger.clone());
    let protocol = ModernProtocolState::default();
    *protocol.stdio_diagnostics.lock().unwrap() = Some(diagnostics.clone());
    (root, logger, diagnostics, protocol)
}
fn lines(logger: &Arc<Mutex<McpLogger>>) -> Vec<String> {
    logger.lock().unwrap().get_ring_buffer()
}

#[test]
fn unclassified_text_waits_for_the_era_and_a_legacy_decision_releases_it() {
    let (_root, logger, diagnostics, protocol) = setup();
    diagnostics.data(&protocol, "booting legacy\npartial");
    assert!(lines(&logger).is_empty(), "unclassified stderr is held");
    protocol.legacy.store(true, Ordering::Release);
    protocol.resolve_stdio_diagnostics(false);
    let logged = lines(&logger);
    assert_eq!(logged.len(), 1);
    assert!(logged[0].contains("booting legacy"));
    // A later chunk completes the held partial line.
    diagnostics.data(&protocol, " line\n");
    assert!(lines(&logger)[1].contains("partial line"));
}

#[test]
fn a_modern_decision_discards_held_text_and_later_chunks() {
    let (_root, logger, diagnostics, protocol) = setup();
    diagnostics.data(&protocol, "probe noise\n");
    protocol.set_version("2026-07-28");
    protocol.resolve_stdio_diagnostics(true);
    diagnostics.data(&protocol, "modern chatter\n");
    diagnostics.end(&protocol);
    assert!(lines(&logger).is_empty());
}

#[test]
fn private_forms_redact_everything_but_allowlisted_error_codes() {
    let (_root, logger, diagnostics, protocol) = setup();
    protocol.private_forms.store(true, Ordering::Release);
    diagnostics.data(
        &protocol,
        "secret answer: hunter2\nspawn failed ENOENT here\n",
    );
    let logged = lines(&logger);
    assert_eq!(logged.len(), 2);
    assert!(logged[0].contains("[private MCP stderr redacted]"));
    assert!(!logged.iter().any(|line| line.contains("hunter2")));
    assert!(logged[1].contains("[private MCP stderr redacted] ENOENT"));
}

#[test]
fn an_unclassified_child_that_exits_releases_its_tail_as_legacy() {
    let (_root, logger, diagnostics, protocol) = setup();
    diagnostics.data(&protocol, "fatal: no config");
    diagnostics.end(&protocol);
    assert!(lines(&logger).is_empty(), "EOF alone never decides the era");
    protocol.resolve_stdio_diagnostics(false);
    let logged = lines(&logger);
    assert_eq!(logged.len(), 1);
    assert!(logged[0].contains("fatal: no config"));
}

#[test]
fn a_legacy_child_flushes_its_unterminated_tail_at_eof() {
    let (_root, logger, diagnostics, protocol) = setup();
    protocol.legacy.store(true, Ordering::Release);
    diagnostics.data(&protocol, "crlf line\r\nno newline");
    diagnostics.end(&protocol);
    let logged = lines(&logger);
    assert_eq!(logged.len(), 2);
    assert!(
        logged[0].contains("\"crlf line\""),
        "the CR is stripped: {}",
        logged[0]
    );
    assert!(logged[1].contains("no newline"));
}

/// modern-mrtr-transport.test.ts: "preserves complete and trailing stderr when an unclassified child exits".
#[tokio::test]
async fn a_real_unclassified_child_that_exits_keeps_complete_and_trailing_stderr() {
    use maho_ext_mcp::transport_sdk::{McpClient, McpTransportSpec};
    let root = tempfile::tempdir().unwrap();
    let logger = Arc::new(Mutex::new(
        McpLogger::new("crash", root.path(), None).unwrap(),
    ));
    let spec = McpTransportSpec::Stdio {
        command: crate::support::node(),
        args: vec![
            "-e".into(),
            "process.stderr.write('startup failure\\ntrailing diagnostic'); process.exitCode = 1;"
                .into(),
        ],
        cwd: None,
        env: Default::default(),
    };
    let client = McpClient::materialize_stdio("crash", &spec, logger.clone())
        .await
        .unwrap();
    let diagnostics = client
        .protocol
        .stdio_diagnostics
        .lock()
        .unwrap()
        .clone()
        .expect("stdio diagnostics");
    tokio::time::timeout(std::time::Duration::from_secs(10), diagnostics.drained())
        .await
        .unwrap();
    let logged = lines(&logger).join("\n");
    assert!(logged.contains("startup failure"), "{logged}");
    assert!(logged.contains("trailing diagnostic"), "{logged}");
    let _ = client.close().await;
}

/// stdio-probe.ts: a late discovery that upgrades a silent child discards what it wrote while unclassified.
#[test]
fn a_late_modern_upgrade_discards_text_held_while_silent() {
    let (_root, logger, diagnostics, protocol) = setup();
    diagnostics.data(&protocol, "modern child chatter\n");
    protocol.set_version("2026-07-28");
    protocol.resolve_stdio_diagnostics(true);
    diagnostics.end(&protocol);
    assert!(lines(&logger).is_empty());
}
