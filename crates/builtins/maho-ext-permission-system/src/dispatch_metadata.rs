//! Port of `permission-system/dispatch-metadata.ts`: the operation an offered tool definition
//! dispatches for an input, and the preflight hook that keeps an approval on it.
//!
//! senpi keys identities on the registered schema object (`WeakMap<parameters, identity>`); the
//! Rust definition carries the resolver itself ([`ToolDefinition::dispatch_identity`]), so the
//! identity is scoped to the offered definition the same way and a replaced offer drops it.
use crate::dispatch::{
    DispatchAuthorizer, DispatchError, DispatchRequest, PreparedApproval, prepare_dispatch_approval,
};
use maho_ext_api::{
    ExtensionContext, ToolCallEvent, ToolDefinition, ToolDispatchIdentity,
    invocation_input::current_invocation_input,
};
use serde_json::Value;
use std::sync::Arc;

/// senpi `registerDispatchIdentity(parameters, identity)`.
pub fn register_dispatch_identity(
    definition: &mut ToolDefinition,
    identity: impl Fn(&Value) -> Option<ToolDispatchIdentity> + Send + Sync + 'static,
) {
    definition.dispatch_identity = Some(Arc::new(identity));
}

/// senpi `getDispatchIdentity(parameters, input)`.
pub fn get_dispatch_identity(
    definition: &ToolDefinition,
    input: &Value,
) -> Option<ToolDispatchIdentity> {
    definition
        .dispatch_identity
        .as_ref()
        .and_then(|identity| identity(input))
}

/// senpi `prepareMcpDispatchApproval`: for an `mcp_` call whose offered definition names its
/// operation, the approval to record once the preflight has approved the call. Other tools keep
/// their existing preflight path, including calls while the runtime cannot resolve tools.
///
/// DIVERGENCE: `lookup` stands in for `pi.getAllTools()` (the live definition by name), and the
/// call's input identity comes from the host-scoped [`current_invocation_input`]; without one the
/// call cannot be fenced and keeps the plain preflight path.
pub fn prepare_mcp_dispatch_approval(
    lookup: impl FnOnce(&str) -> Option<ToolDefinition>,
    event: &ToolCallEvent,
    ctx: &ExtensionContext,
    authorizer: &dyn DispatchAuthorizer,
) -> Result<Option<PreparedApproval>, DispatchError> {
    if !event.tool_name.starts_with("mcp_") {
        return Ok(None);
    }
    let Some(identity) = lookup(&event.tool_name)
        .and_then(|definition| get_dispatch_identity(&definition, &event.input))
    else {
        return Ok(None);
    };
    let Some(input_identity) = current_invocation_input() else {
        return Ok(None);
    };
    let request = DispatchRequest {
        tool_call_id: event.tool_call_id.clone(),
        parent_tool_call_id: None,
        tool_name: event.tool_name.clone(),
        input: event.input.clone(),
        input_identity,
        identity,
    };
    let policy = authorizer
        .policy(&request, ctx)
        .map_err(DispatchError::Failed)?;
    prepare_dispatch_approval(&ctx.session_manager, &request, policy).map(Some)
}
