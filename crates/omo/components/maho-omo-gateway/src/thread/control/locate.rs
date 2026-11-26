//! Single address-to-SessionControl lookup (`thread/control/locate.ts`).

use std::sync::Arc;

use serde_json::{Value, json};

use super::super::address::{ResolveContext, ResolveOptions, ResolveWhere, resolve_address};
use super::super::addressing::workspace_entries_async;
use super::super::contracts::results::ThreadSummary;
use super::super::errors::thread_tool_failure;
use super::super::tools::internals::{send_book_entries_async, target_session};
use super::super::tools::ports::{ThreadHostView, ThreadToolSurfaceOptions};
use super::local_control::create_local_control;
use super::port::SessionControl;

pub enum LocatedControl {
    Control(Arc<dyn SessionControl>),
    NotLive {
        thread_id: String,
        title: String,
        device: Option<String>,
    },
    Error(Value),
}

pub async fn locate_control(
    options: &ThreadToolSurfaceOptions,
    current: &ThreadHostView,
    address: &str,
    all_scope: bool,
    caller_id: &str,
) -> LocatedControl {
    let entries = send_book_entries_async(options, current, address).await;
    let root = (options.caller_workspace_root)();
    let ctx = ResolveContext {
        remote: options.remote.clone(),
        caller_workspace_root: root.clone(),
        local: entries.clone(),
        local_for: None,
        caller_id: caller_id.to_owned(),
    };
    let opts = ResolveOptions {
        all_scope,
        probe: true,
        need: Some(super::super::address::types::ResolveNeed::Online),
        ..Default::default()
    };
    match resolve_address(&ctx, address, &opts).await {
        Err(err) => LocatedControl::Error(json!({ "kind": "error", "error": err })),
        Ok(resolved) => match resolved.where_located {
            ResolveWhere::Device { row } => LocatedControl::Error(json!({
                "kind": "error",
                "error": thread_tool_failure(
                    "remote_unavailable",
                    format!("Thread {} is on device {}, which is unavailable without remote relay (O02).", row.session_id, row.device),
                    "Target a thread on this device.",
                    Some(json!({ "device": row.device, "thread": row.session_id })),
                )
            })),
            ResolveWhere::Here { .. } => {
                let Some(session) = target_session(current, &resolved.thread).cloned() else {
                    return LocatedControl::NotLive {
                        thread_id: resolved.thread,
                        title: resolved.entry.summary.name,
                        device: None,
                    };
                };
                let visible_entries: Vec<ThreadSummary> = if all_scope {
                    entries.into_iter().map(|e| e.summary).collect()
                } else {
                    workspace_entries_async(&entries, Some(&root))
                        .await
                        .into_iter()
                        .map(|e| e.summary)
                        .collect()
                };
                let title = resolved.entry.summary.name.clone();
                let control = create_local_control(
                    options,
                    session,
                    resolved.thread,
                    title,
                    Arc::new(move || visible_entries.clone()),
                );
                LocatedControl::Control(control)
            }
        },
    }
}
