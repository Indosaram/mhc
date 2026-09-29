use std::sync::{Arc, LazyLock, RwLock};

use super::types::BoxFuture;

pub const LIVE_ROUTE_DISPATCH_LOG: &str = "[live-server-route] dispatch via live listener";
pub const LIVE_ROUTE_UNAVAILABLE_LOG: &str =
    "[live-server-route] route unavailable; using in-process client";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PromptDispatchRoute {
    Live,
    #[default]
    InProcess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PromptDispatchRouteReason {
    #[default]
    Identity,
    Flag,
    Child,
    Unavailable,
    Live,
    Affinity,
}

pub type BoxDispatchRefFn = Arc<
    dyn Fn(&serde_json::Value) -> BoxFuture<'static, Result<serde_json::Value, String>>
        + Send
        + Sync,
>;

#[derive(Clone, Default)]
pub struct PromptDispatchLiveClient {
    pub prompt_async: Option<BoxDispatchRefFn>,
    pub prompt: Option<BoxDispatchRefFn>,
}

#[derive(Clone, Default)]
pub struct PromptDispatchRouteResult {
    pub route: PromptDispatchRoute,
    pub reason: PromptDispatchRouteReason,
    pub live_client: Option<PromptDispatchLiveClient>,
}

pub trait PromptDispatchRouteResolver: Send + Sync {
    fn try_resolve_dispatch_client_sync(
        &self,
        _session_id: &str,
    ) -> Option<PromptDispatchRouteResult> {
        None
    }

    fn resolve_dispatch_client(
        &self,
        _session_id: &str,
    ) -> BoxFuture<'static, PromptDispatchRouteResult> {
        Box::pin(async {
            PromptDispatchRouteResult {
                route: PromptDispatchRoute::InProcess,
                reason: PromptDispatchRouteReason::Identity,
                live_client: None,
            }
        })
    }

    fn is_pre_send_connection_failure(&self, _error: &str) -> bool {
        false
    }

    fn mark_live_route_unavailable(&self, _reason: &str) {}
}

static ROUTE_RESOLVER: LazyLock<RwLock<Option<Arc<dyn PromptDispatchRouteResolver>>>> =
    LazyLock::new(|| RwLock::new(None));

pub fn configure_prompt_dispatch_route_resolver(
    resolver: Option<Arc<dyn PromptDispatchRouteResolver>>,
) {
    *ROUTE_RESOLVER
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = resolver;
}

pub fn try_resolve_dispatch_client_sync(session_id: &str) -> Option<PromptDispatchRouteResult> {
    let resolver = ROUTE_RESOLVER
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    match resolver {
        Some(r) => r.try_resolve_dispatch_client_sync(session_id),
        None => Some(PromptDispatchRouteResult {
            route: PromptDispatchRoute::InProcess,
            reason: PromptDispatchRouteReason::Identity,
            live_client: None,
        }),
    }
}

pub async fn resolve_dispatch_client(session_id: &str) -> PromptDispatchRouteResult {
    let resolver = ROUTE_RESOLVER
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    match resolver {
        Some(r) => r.resolve_dispatch_client(session_id).await,
        None => PromptDispatchRouteResult {
            route: PromptDispatchRoute::InProcess,
            reason: PromptDispatchRouteReason::Identity,
            live_client: None,
        },
    }
}

pub fn is_pre_send_connection_failure(error: &str) -> bool {
    let resolver = ROUTE_RESOLVER
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    resolver.is_some_and(|r| r.is_pre_send_connection_failure(error))
}

pub fn mark_live_route_unavailable(reason: &str) {
    let resolver = ROUTE_RESOLVER
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    if let Some(r) = resolver {
        r.mark_live_route_unavailable(reason);
    }
}
