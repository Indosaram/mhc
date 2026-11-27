//! Published durable-id single-target lookup helper (`thread/tools/send-view.ts`).

use super::internals::host_view;
use super::ports::{ThreadHostView, ThreadHostViewRequest, ThreadToolSurfaceOptions};

/// Resolves a published session owner directly via `list_target`, falling back to `legacy` when absent.
pub async fn send_view(
    options: &ThreadToolSurfaceOptions,
    address: &str,
    offline: bool,
) -> Result<ThreadHostView, String> {
    let owner = options.store.session_endpoint(address).unwrap_or(None);
    let Some((_, socket, kind_raw)) = owner else {
        return host_view(options, ThreadHostViewRequest { offline }).await;
    };
    if socket.is_empty() {
        return Ok(ThreadHostView::default());
    }
    let kind = if kind_raw == "tui" {
        super::super::endpoint_registry::EndpointKind::Tui
    } else {
        super::super::endpoint_registry::EndpointKind::RpcHost
    };
    if let Some(view) = options.host.list_target(address, &socket, kind).await? {
        return Ok(view);
    }
    Ok(ThreadHostView::default())
}
