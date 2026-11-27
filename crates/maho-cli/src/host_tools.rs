//! Port of `packages/coding-agent/src/host-tools.ts` (senpi#5045).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use maho_ai::utils::abort::AbortSignal;
use maho_ext_api::{
    AgentToolResult, ExtensionApi, ExtensionContext, ExtensionFailure, ExtensionFuture,
    ExtensionToolExecutor, ToolDefinition,
};
use serde_json::Value;

/// Event-bus channel on which a host bridge sets or clears the route for one tool name.
pub const HOST_TOOL_ROUTE_CHANNEL: &str = "senpi:host-tool-route";
/// Error message prefix/tag a route returns when its host cannot serve the call; the call then runs
/// on the standalone tool implementation.
pub const HOST_TOOL_ROUTE_UNAVAILABLE: &str = "HostToolRouteUnavailable";

pub type SettleRouted = Arc<
    dyn for<'a> Fn(
            AgentToolResult,
            &'a str,
            Value,
            &'a ExtensionContext,
        ) -> ExtensionFuture<'a, AgentToolResult>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct HostToolRouteUpdate {
    pub name: String,
    pub execute: Option<ExtensionToolExecutor>,
}

pub struct HostTool {
    pub tool: ToolDefinition,
    pub execute: ExtensionToolExecutor,
    pub settle_routed: Option<SettleRouted>,
}

pub fn is_route_unavailable(error: &ExtensionFailure) -> bool {
    error.message == HOST_TOOL_ROUTE_UNAVAILABLE
        || error
            .message
            .starts_with(&format!("{HOST_TOOL_ROUTE_UNAVAILABLE}:"))
}

fn is_aborted(signal: &Option<AbortSignal>) -> bool {
    signal.as_ref().is_some_and(AbortSignal::aborted)
}

/// Attaches host tools through the extension host SDK (`ExtensionApi`).
/// Each tool is registered once under its own name, and routes via `HOST_TOOL_ROUTE_CHANNEL`
/// (`on_native::<HostToolRouteUpdate>`) before falling back to standalone execution on unavailable.
pub fn attach_host_tools(api: &mut ExtensionApi, tools: Vec<HostTool>) {
    for HostTool {
        tool,
        execute: standalone,
        settle_routed,
    } in tools
    {
        let tool_name = tool.name.clone();
        let route: Arc<Mutex<Option<(u64, ExtensionToolExecutor)>>> = Arc::default();
        let next_generation = Arc::new(AtomicU64::new(1));
        let route_for_bus = route.clone();
        let gen_for_bus = next_generation.clone();
        let bus_name = tool_name.clone();
        let _sub = api.events.on_native::<HostToolRouteUpdate>(
            HOST_TOOL_ROUTE_CHANNEL,
            Arc::new(move |update| {
                if update.name != bus_name {
                    return;
                }
                let next = update.execute.as_ref().map(|exec| {
                    let generation = gen_for_bus.fetch_add(1, Ordering::Relaxed);
                    (generation, exec.clone())
                });
                *route_for_bus.lock().unwrap_or_else(PoisonError::into_inner) = next;
            }),
        );
        let route_for_exec = route.clone();
        let wrapped: ExtensionToolExecutor = Arc::new(move |id, params, signal, on_update, ctx| {
            let route_for_exec = route_for_exec.clone();
            let standalone = standalone.clone();
            let settle_routed = settle_routed.clone();
            Box::pin(async move {
                let current = route_for_exec
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                let Some((generation, routed)) = current else {
                    return standalone(id, params, signal, on_update, ctx).await;
                };
                match routed(id, params.clone(), signal.clone(), on_update.clone(), ctx).await {
                    Ok(result) => match &settle_routed {
                        Some(settle) => settle(result, id, params, ctx).await,
                        None => Ok(result),
                    },
                    Err(error) if is_route_unavailable(&error) => {
                        {
                            let mut guard = route_for_exec
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner);
                            if guard
                                .as_ref()
                                .is_some_and(|(gen_id, _)| *gen_id == generation)
                            {
                                *guard = None;
                            }
                        }
                        if is_aborted(&signal) {
                            return Err(ExtensionFailure::new("The operation was aborted"));
                        }
                        standalone(id, params, signal, on_update, ctx).await
                    }
                    Err(error) => Err(error),
                }
            })
        });
        if let Err(error) = api.register_tool_with_extension_context(tool, wrapped) {
            std::panic::panic_any(error);
        }
    }
}
