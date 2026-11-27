//! Monitor target preflight resolution (`thread/tools/monitor-target.ts`).

use serde_json::Value;

use super::super::address::{ResolveContext, ResolveOptions, ResolveWhere, resolve_address};
use super::super::errors::{ThreadToolFailure, thread_tool_failure};
use super::internals::{host_view, send_book_entries_async};
use super::ports::{ThreadHostViewRequest, ThreadToolSurfaceOptions};

#[derive(Debug, Clone, PartialEq)]
pub enum MonitorTargetResolution {
    Keep,
    Target(String),
    Refused(ThreadToolFailure),
}

pub async fn resolve_monitor_target(
    options: &ThreadToolSurfaceOptions,
    target: &str,
    caller_id: &str,
) -> MonitorTargetResolution {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return MonitorTargetResolution::Keep;
    }
    if trimmed.contains('/') {
        return MonitorTargetResolution::Refused(thread_tool_failure(
            "remote_unavailable",
            format!(
                "Monitor target \"{trimmed}\" names another device, which is unavailable without remote relay (O02)."
            ),
            "Monitor a local thread on this device.",
            None,
        ));
    }
    if let Some(ensure) = &options.ensure_host
        && let Err(err) = ensure().await
    {
        return MonitorTargetResolution::Refused(thread_tool_failure(
            "host_unavailable",
            format!("Thread host unavailable: {err}"),
            "Retry when the host is running.",
            None,
        ));
    }
    let view = match host_view(options, ThreadHostViewRequest { offline: true }).await {
        Ok(v) => v,
        Err(err) => {
            return MonitorTargetResolution::Refused(thread_tool_failure(
                "host_unavailable",
                format!("Thread host unavailable: {err}"),
                "Retry when the host is running.",
                None,
            ));
        }
    };
    let entries = send_book_entries_async(options, &view, trimmed).await;
    let ctx = ResolveContext {
        remote: options.remote.clone(),
        caller_workspace_root: (options.caller_workspace_root)(),
        local: entries,
        local_for: None,
        caller_id: caller_id.to_owned(),
    };
    let opts = ResolveOptions {
        all_scope: true,
        probe: true,
        need: Some(super::super::address::types::ResolveNeed::Online),
        ..Default::default()
    };
    match resolve_address(&ctx, trimmed, &opts).await {
        Err(err) => MonitorTargetResolution::Refused(err.into_failure()),
        Ok(res) => match res.where_located {
            ResolveWhere::Here { .. } => MonitorTargetResolution::Target(res.thread),
            ResolveWhere::Device { row } => MonitorTargetResolution::Refused(thread_tool_failure(
                "remote_unavailable",
                format!(
                    "Thread {} is on device {}, which requires O02.",
                    row.session_id, row.device
                ),
                "Monitor a local thread.",
                None,
            )),
        },
    }
}

pub fn monitor_rpc_target_from_input(tool_name: &str, input: &Value) -> Option<String> {
    if tool_name != "monitor" {
        return None;
    }
    if input.get("type").and_then(Value::as_str) != Some("rpc") {
        return None;
    }
    input
        .get("target")
        .and_then(Value::as_str)
        .map(str::to_owned)
}
