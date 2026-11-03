//! Port of senpi mcp/request-origin.ts: binding-issued request ownership for MRTR and private legacy forms.
use crate::{
    connection::ServerConnection,
    form_types::{McpFormPresentation, McpFormServerSource},
    modern::errors::{McpMrtrError, McpMrtrErrorCode},
    transport_sdk::McpClient,
    wrap::McpLinkedSignal,
};
use futures::future::BoxFuture;
use maho_core::mcp_mrtr_presentations::McpMrtrPresentation;
use maho_ext_api::AbortSignal;
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

pub type McpAuthorityResolver = Arc<
    dyn Fn(AbortSignal) -> BoxFuture<'static, Result<Value, crate::errors::McpError>> + Send + Sync,
>;

/// Upstream `requestOrigin?: () => McpRequestOrigin | undefined`: a fresh origin per request, so presentation
/// and lifetime are read at call time.
pub type McpRequestOriginFactory = Arc<dyn Fn() -> Option<Arc<McpRequestOrigin>> + Send + Sync>;

pub struct McpRequestOrigin {
    /// Identity of the session binding that issued this origin (upstream: the binding object).
    pub binding: usize,
    pub connection: Arc<ServerConnection>,
    pub signal: AbortSignal,
    pub presentation: Option<McpMrtrPresentation>,
    pub private_form: Option<McpFormPresentation>,
    pub private_forms_enforced: bool,
    pub server_label: String,
    pub server_source: Option<McpFormServerSource>,
    pub is_current: Arc<dyn Fn() -> bool + Send + Sync>,
    pub resolve_authority: McpAuthorityResolver,
}
impl McpRequestOrigin {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        binding: usize,
        connection: Arc<ServerConnection>,
        signal: AbortSignal,
        presentation: Option<McpMrtrPresentation>,
        private_form: Option<McpFormPresentation>,
        private_forms_enforced: bool,
        server_label: String,
        server_source: Option<McpFormServerSource>,
        is_current: Arc<dyn Fn() -> bool + Send + Sync>,
        resolve_authority: McpAuthorityResolver,
    ) -> Self {
        Self {
            binding,
            connection,
            signal,
            presentation,
            private_form,
            private_forms_enforced,
            server_label,
            server_source,
            is_current,
            resolve_authority,
        }
    }
    pub fn current(&self) -> bool {
        (self.is_current)()
    }
}

/// Issued by the binding-specific registration boundary, never from JSON/model arguments.
// DIVERGENCE(senpi request-origin.ts origins WeakSet / registerMcpClientAlias): a Rust origin is a typed value
// that model JSON cannot forge, and shared leases hand out the same `Arc<McpClient>`, so neither the issued-set
// check nor client aliases are needed; `take_mcp_request_origin` still compares the physical client.
pub fn create_mcp_request_origin(origin: McpRequestOrigin) -> Arc<McpRequestOrigin> {
    Arc::new(origin)
}

/// Upstream `mcpRequestOptions`: what a caller hands the request engine alongside method and params.
#[derive(Clone, Default)]
pub struct McpRequestOptions {
    pub origin: Option<Arc<McpRequestOrigin>>,
    pub signal: Option<AbortSignal>,
    pub max_total_timeout: Option<std::time::Duration>,
    /// Upstream `onprogress`, for task status the engine synthesizes.
    pub on_progress: Option<Arc<dyn Fn(Value) + Send + Sync>>,
}
pub fn mcp_request_options(
    origin: Option<Arc<McpRequestOrigin>>,
    signal: Option<AbortSignal>,
) -> McpRequestOptions {
    McpRequestOptions {
        origin,
        signal,
        ..Default::default()
    }
}

/// Validates the origin against the physical client the request is about to use.
pub fn take_mcp_request_origin(
    options: &McpRequestOptions,
    client: &Arc<McpClient>,
) -> Result<Option<Arc<McpRequestOrigin>>, McpMrtrError> {
    let Some(origin) = options.origin.clone() else {
        return Ok(None);
    };
    let changed = || McpMrtrError::new(McpMrtrErrorCode::ContextChanged);
    if !origin.current() {
        return Err(changed());
    }
    let candidate = origin.connection.client().map_err(|_| changed())?;
    if !Arc::ptr_eq(&candidate, client) {
        return Err(changed());
    }
    Ok(Some(origin))
}

pub struct LegacyMcpOperation {
    id: u64,
    pub origin: Arc<McpRequestOrigin>,
    pub signal: AbortSignal,
    lifetime: AbortSignal,
    _linked: McpLinkedSignal,
}
static NEXT_OPERATION: AtomicU64 = AtomicU64::new(1);

/// Ends the legacy operation when dropped.
pub struct LegacyMcpOperationGuard {
    client: Arc<McpClient>,
    id: u64,
}
impl Drop for LegacyMcpOperationGuard {
    fn drop(&mut self) {
        let mut operations = self
            .client
            .legacy_operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = operations
            .iter()
            .position(|operation| operation.id == self.id)
        {
            operations.remove(index).lifetime.abort();
        }
    }
}
pub fn begin_legacy_mcp_operation(
    client: &Arc<McpClient>,
    origin: Arc<McpRequestOrigin>,
    caller: Option<&AbortSignal>,
) -> LegacyMcpOperationGuard {
    let lifetime = AbortSignal::default();
    let mut sources = vec![origin.signal.clone(), lifetime.clone()];
    sources.extend(caller.cloned());
    let linked = McpLinkedSignal::new(sources);
    let id = NEXT_OPERATION.fetch_add(1, Ordering::Relaxed);
    client
        .legacy_operations
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(LegacyMcpOperation {
            id,
            origin,
            signal: linked.signal.clone(),
            lifetime,
            _linked: linked,
        });
    LegacyMcpOperationGuard {
        client: client.clone(),
        id,
    }
}

pub enum LegacyMcpOperationLookup {
    None,
    Ambiguous,
    One {
        origin: Arc<McpRequestOrigin>,
        signal: AbortSignal,
    },
}
pub fn legacy_mcp_operation(client: &McpClient) -> LegacyMcpOperationLookup {
    let operations = client
        .legacy_operations
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match operations.as_slice() {
        [] => LegacyMcpOperationLookup::None,
        [operation] if !operation.signal.is_aborted() && operation.origin.current() => {
            LegacyMcpOperationLookup::One {
                origin: operation.origin.clone(),
                signal: operation.signal.clone(),
            }
        }
        [_] => LegacyMcpOperationLookup::None,
        _ => LegacyMcpOperationLookup::Ambiguous,
    }
}
