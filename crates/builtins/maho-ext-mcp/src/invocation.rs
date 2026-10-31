//! Port of senpi mcp/invocation.ts: resolve one MCP call against the live session before it is
//! sent. The resolver waits for the server's metadata, re-reads the current catalog entry,
//! re-validates the arguments against its raw schema, checks that the invoked tool name still
//! identifies this operation, and asks the permission system's dispatch authorizer. The returned
//! [`McpReadyInvocation::is_current`] is the fence checked immediately before the send.
use crate::{
    catalog::{McpCatalogCallback, McpToolCatalogEntry},
    errors::{McpError, McpErrorKind},
    modern::tool_schema::{ModernSchemaValidator, compile_modern_schema},
};
use maho_ext_api::{
    AbortSignal, ExtensionContext, ToolCall, ToolDefinition, ToolDispatchIdentity,
    ToolDispatchOwner, ToolError,
    invocation_input::{InvocationInput, current_invocation_input},
};
use maho_ext_permission_system::{
    dispatch::{DispatchFence, DispatchRequest, authorize_tool_dispatch, has_dispatch_authorizer},
    dispatch_metadata::get_dispatch_identity,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// senpi `McpInvocation`: the agent-level call that reached an MCP tool body.
pub struct McpInvocation<'a> {
    pub tool_call_id: String,
    /// The registered tool name the call was made through (a direct tool, stub or proxy gateway).
    pub tool_name: String,
    /// The validated input of that registered tool.
    pub input: Value,
    /// The host-scoped identity of that input (senpi's input object), when the host scopes one.
    pub input_identity: Option<InvocationInput>,
    pub context: Option<&'a ExtensionContext>,
}
impl<'a> McpInvocation<'a> {
    /// The invocation of `call` through the registered tool `tool_name` with input `input`.
    pub fn from_call(call: &ToolCall<'a>, tool_name: &str, input: Value) -> Self {
        Self {
            tool_call_id: call.id.to_owned(),
            tool_name: tool_name.to_owned(),
            input,
            input_identity: current_invocation_input(),
            context: call
                .context
                .and_then(|context| context.as_any())
                .and_then(|context| context.downcast_ref::<ExtensionContext>()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpInvocationErrorKind {
    Unavailable,
    SchemaChanged,
    PermissionDenied,
}
impl McpInvocationErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::SchemaChanged => "schema_changed",
            Self::PermissionDenied => "permission_denied",
        }
    }
}

/// senpi `McpInvocationError` (field order matches its object literal for `JSON.stringify`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpInvocationError {
    pub kind: McpInvocationErrorKind,
    pub message: String,
    pub server: String,
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_definition: Option<Value>,
}

/// The current offered entry for a tool. `current` waits for an in-flight catalog change;
/// `current_now` never waits and reports a changing catalog as not current.
pub type McpInvocationCurrent = Arc<
    dyn Fn(String) -> futures::future::BoxFuture<'static, Option<McpToolCatalogEntry>>
        + Send
        + Sync,
>;
pub type McpInvocationCurrentNow = Arc<dyn Fn(&str) -> Option<McpToolCatalogEntry> + Send + Sync>;
/// The live registered definition by name (senpi `ctx.tools.find`).
pub type McpToolLookup = Arc<dyn Fn(&str) -> Option<ToolDefinition> + Send + Sync>;

/// senpi `InvocationScope`.
///
/// DIVERGENCE: senpi compares `ctx.sessionManager` objects; the host rebuilds that adapter on a
/// rebind, so the session is compared by id. `lookup` stands in for `ctx.tools`, which the Rust
/// context does not carry; without one the operation-name fence is skipped.
pub struct McpInvocationScope {
    pub owner: ToolDispatchOwner,
    pub configuration: String,
    pub context_required: bool,
    pub session_id: Option<String>,
    pub is_current: Arc<dyn Fn() -> bool + Send + Sync>,
    pub ready: McpCatalogCallback,
    pub current: McpInvocationCurrent,
    pub current_now: McpInvocationCurrentNow,
    pub lookup: Option<McpToolLookup>,
}

/// senpi `McpInvocationResolver`.
pub struct McpInvocationResolver {
    scope: Arc<McpInvocationScope>,
}

/// senpi `createMcpInvocationResolver`.
pub fn create_mcp_invocation_resolver(scope: McpInvocationScope) -> McpInvocationResolver {
    McpInvocationResolver {
        scope: Arc::new(scope),
    }
}

fn identity_of(scope: &McpInvocationScope, entry: &McpToolCatalogEntry) -> ToolDispatchIdentity {
    ToolDispatchIdentity {
        owner: scope.owner.clone(),
        operation: Some(json!([entry.server, entry.tool]).to_string()),
        metadata: json!([
            scope.configuration,
            entry.server,
            entry.tool,
            entry.schema,
            entry.output_schema,
            entry.modern,
            entry.description,
            entry.annotations,
        ])
        .to_string(),
    }
}

/// The current raw schema's check, never the coercing preflight schema.
enum ArgumentCheck {
    Modern(ModernSchemaValidator),
    Legacy(Value),
}
impl ArgumentCheck {
    fn check(&self, args: &Value) -> bool {
        match self {
            Self::Modern(validator) => validator.validate(args),
            // `Check(Type.Unsafe(schema), value)`; a missing schema constrains nothing.
            Self::Legacy(Value::Null) => true,
            Self::Legacy(schema) => {
                maho_ai::utils::validation::strict_errors(schema, args).is_empty()
            }
        }
    }
}

/// The registered tool still dispatches this operation for the call's input.
struct OperationFence {
    lookup: McpToolLookup,
    tool_name: String,
    input: Value,
    owner: ToolDispatchOwner,
    operation: Option<String>,
}
impl OperationFence {
    fn holds(&self) -> bool {
        (self.lookup)(&self.tool_name)
            .and_then(|definition| get_dispatch_identity(&definition, &self.input))
            .is_some_and(|identity| {
                identity.owner == self.owner && identity.operation == self.operation
            })
    }
}

/// senpi `McpPreparedInvocation` `ready`.
pub struct McpReadyInvocation {
    pub entry: McpToolCatalogEntry,
    scope: Arc<McpInvocationScope>,
    tool: String,
    args: Value,
    signal: Option<AbortSignal>,
    metadata: String,
    check: ArgumentCheck,
    operation: Option<OperationFence>,
    permission: Option<(DispatchFence, DispatchRequest)>,
}
impl McpReadyInvocation {
    /// The fence checked immediately before the send: nothing the call was resolved against has
    /// changed since.
    pub fn is_current(&self) -> bool {
        if self.signal.as_ref().is_some_and(AbortSignal::is_aborted) || !(self.scope.is_current)() {
            return false;
        }
        let Some(latest) = (self.scope.current_now)(&self.tool) else {
            return false;
        };
        identity_of(&self.scope, &latest).metadata == self.metadata
            && self.check.check(&self.args)
            && self.operation.as_ref().is_none_or(OperationFence::holds)
            && self
                .permission
                .as_ref()
                .is_none_or(|(fence, request)| fence.is_current(request))
    }
}

pub enum McpPreparedInvocation {
    Ready(Box<McpReadyInvocation>),
    Refused(McpInvocationError),
}

fn aborted_error() -> McpError {
    McpError::new(McpErrorKind::ToolExec, ToolError::Aborted.to_string())
}
fn check_signal(signal: Option<&AbortSignal>) -> Result<(), McpError> {
    if signal.is_some_and(AbortSignal::is_aborted) {
        Err(aborted_error())
    } else {
        Ok(())
    }
}
async fn aborted(signal: Option<&AbortSignal>) {
    match signal {
        Some(signal) => signal.cancelled().await,
        None => std::future::pending().await,
    }
}

impl McpInvocationResolver {
    /// senpi `identity(entry)`: the operation an offered entry dispatches.
    pub fn identity(&self, entry: &McpToolCatalogEntry) -> ToolDispatchIdentity {
        identity_of(&self.scope, entry)
    }

    /// senpi `resolve`: a refusal is a result; an abort, a readiness failure or the metadata
    /// deadline is an error.
    pub async fn resolve(
        &self,
        offered: &McpToolCatalogEntry,
        args: &Value,
        invocation: Option<&McpInvocation<'_>>,
        signal: Option<&AbortSignal>,
    ) -> Result<McpPreparedInvocation, McpError> {
        let scope = &self.scope;
        let refuse = |kind, message: &str, current: Option<&McpToolCatalogEntry>| {
            McpPreparedInvocation::Refused(McpInvocationError {
                kind,
                message: message.to_owned(),
                server: offered.server.clone(),
                tool: offered.tool.clone(),
                current_definition: current.map(|current| current.schema.clone()),
            })
        };
        const REPLACED: &str = "MCP session or server configuration was replaced.";
        check_signal(signal)?;
        if !(scope.is_current)() {
            return Ok(refuse(McpInvocationErrorKind::Unavailable, REPLACED, None));
        }
        let ctx = invocation.and_then(|invocation| invocation.context);
        if scope.context_required
            && ctx.is_none_or(|ctx| {
                scope
                    .session_id
                    .as_deref()
                    .is_some_and(|id| ctx.session_manager.session_id() != id)
            })
        {
            return Ok(refuse(
                McpInvocationErrorKind::Unavailable,
                "MCP invocation does not belong to this session.",
                None,
            ));
        }
        tokio::select! {
            biased;
            () = aborted(signal) => return Err(aborted_error()),
            () = tokio::time::sleep(offered.request_timeout) => {
                let mut error = McpError::new(
                    McpErrorKind::Timeout,
                    "MCP metadata was not ready before the request timeout.",
                );
                error.phase = Some("catalog".into());
                error.server_name = Some(offered.server.clone());
                return Err(error);
            }
            ready = (scope.ready)() => ready?,
        }
        check_signal(signal)?;
        if !(scope.is_current)() {
            return Ok(refuse(McpInvocationErrorKind::Unavailable, REPLACED, None));
        }
        let Some(current) = (scope.current)(offered.tool.clone()).await else {
            return Ok(refuse(
                McpInvocationErrorKind::Unavailable,
                "MCP tool is no longer available.",
                None,
            ));
        };
        let check = if current.modern {
            match compile_modern_schema(&current.schema) {
                Ok(validator) => ArgumentCheck::Modern(validator),
                Err(error) => {
                    return Ok(refuse(
                        McpInvocationErrorKind::SchemaChanged,
                        &format!("{}: {}", error.code.as_str(), error.message),
                        Some(&current),
                    ));
                }
            }
        } else {
            ArgumentCheck::Legacy(current.schema.clone())
        };
        if !check.check(args) {
            return Ok(refuse(
                McpInvocationErrorKind::SchemaChanged,
                "Arguments do not match the current MCP tool schema.",
                Some(&current),
            ));
        }
        let prepared_identity = identity_of(scope, &current);
        let mut operation = None;
        let mut permission = None;
        // The Rust context always carries a session manager (senpi's second disjunct).
        if let (Some(ctx), Some(invocation)) = (ctx, invocation) {
            if let Some(lookup) = &scope.lookup {
                let fence = OperationFence {
                    lookup: Arc::clone(lookup),
                    tool_name: invocation.tool_name.clone(),
                    input: invocation.input.clone(),
                    owner: prepared_identity.owner.clone(),
                    operation: prepared_identity.operation.clone(),
                };
                if !fence.holds() {
                    return Ok(refuse(
                        McpInvocationErrorKind::Unavailable,
                        "MCP invocation name no longer identifies this operation.",
                        Some(&current),
                    ));
                }
                operation = Some(fence);
            }
            if has_dispatch_authorizer(&ctx.session_manager) {
                let request = DispatchRequest {
                    tool_call_id: invocation.tool_call_id.clone(),
                    parent_tool_call_id: None,
                    tool_name: invocation.tool_name.clone(),
                    input: invocation.input.clone(),
                    input_identity: invocation.input_identity.clone().unwrap_or_default(),
                    identity: prepared_identity.clone(),
                };
                match authorize_tool_dispatch(&ctx.session_manager, &request, ctx, signal.cloned())
                    .await
                {
                    Ok(fence) => permission = Some((fence, request)),
                    Err(error) => {
                        check_signal(signal)?;
                        return Ok(refuse(
                            McpInvocationErrorKind::PermissionDenied,
                            &error.to_string(),
                            Some(&current),
                        ));
                    }
                }
            } else if ctx
                .loaded_extension_paths
                .iter()
                .any(|path| path.contains("permission-system"))
            {
                // DIVERGENCE: senpi also refuses when `loadedExtensionPaths` is undefined; the Rust
                // context always carries the list, so only a loaded permission system without an
                // authorizer refuses here.
                return Ok(refuse(
                    McpInvocationErrorKind::PermissionDenied,
                    "Permission authorizer is unavailable.",
                    Some(&current),
                ));
            }
        }
        Ok(McpPreparedInvocation::Ready(Box::new(McpReadyInvocation {
            entry: current,
            scope: Arc::clone(scope),
            tool: offered.tool.clone(),
            args: args.clone(),
            signal: signal.cloned(),
            metadata: prepared_identity.metadata,
            check,
            operation,
            permission,
        })))
    }
}
