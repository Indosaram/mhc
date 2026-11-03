//! Port of senpi mcp/modern/stdio-probe.ts: probe the started child before `initialize`.
//!
//! A modern child keeps its process: `initialize` is answered locally from discovery and every later request
//! carries the per-request `_meta` at the negotiated version. A clean non-modern error reply, or no reply in
//! time (a slow start, or a server that ignores unknown requests), keeps the same process for the legacy
//! handshake; a discovery reply that arrives late still upgrades it to modern. Only a child that exited or
//! wrote non-JSON to stdout is replaced by a fresh process for the legacy handshake.
use super::era::{EraOutcome, ProbeOptions, probe_stdio_era};
use crate::{
    errors::{McpError, McpErrorKind},
    log::McpLogger,
    transport_sdk::McpClient,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

/// How long a stdio child gets to answer discovery (at most a quarter of the connect timeout).
pub const STDIO_PROBE_MS: u64 = 2_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StdioProbeResult {
    /// The client now speaks the modern wire at this version.
    Modern(String),
    /// Same process, legacy handshake next (`silent`: a late discovery may still upgrade it).
    Legacy { silent: bool },
    /// The probed child is unusable; spawn a fresh one for the legacy handshake.
    Restart,
}

pub async fn probe_stdio_transport(
    client: &Arc<McpClient>,
    options: &ProbeOptions,
    logger: &Arc<Mutex<McpLogger>>,
    timeout: Duration,
    server_name: &str,
    probe_ms: Option<u64>,
) -> Result<StdioProbeResult, McpError> {
    let wait = Duration::from_millis(probe_ms.unwrap_or(STDIO_PROBE_MS)).min(timeout / 4);
    let outcome = probe_stdio_era(|request| client.probe_exchange(request, wait), options).await;
    let log = |level: &str, message: &str| {
        let _ = logger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .log(level, message, None, None);
    };
    match outcome {
        EraOutcome::LegacyFallback { restart: true, .. } => {
            log(
                "info",
                &format!(
                    "MCP server {server_name}: no clean discovery answer; restarting it for the legacy handshake"
                ),
            );
            Ok(StdioProbeResult::Restart)
        }
        EraOutcome::LegacyFallback { silent, .. } => {
            if silent {
                client.await_late_discovery();
            }
            Ok(StdioProbeResult::Legacy { silent })
        }
        EraOutcome::Modern {
            version,
            discovery: Some(discovery),
            ..
        } => {
            client.speak_modern(&discovery, &version, None)?;
            Ok(StdioProbeResult::Modern(version))
        }
        EraOutcome::Modern { error, .. } => fail(
            server_name,
            error.map_or_else(String::new, |error| error.message),
            &log,
        ),
        EraOutcome::Indeterminate { reason, action, .. } => {
            fail(server_name, format!("{reason}. {action}"), &log)
        }
    }
}

fn fail(
    server_name: &str,
    reason: String,
    log: &dyn Fn(&str, &str),
) -> Result<StdioProbeResult, McpError> {
    let message = format!("MCP server {server_name} modern discovery failed: {reason}");
    log("error", &message);
    let mut error = McpError::new(McpErrorKind::Connect, message);
    error.phase = Some("modern-stdio".into());
    error.server_name = Some(server_name.into());
    Err(error)
}
