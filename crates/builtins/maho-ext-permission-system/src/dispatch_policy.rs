//! Port of `permission-system/dispatch-policy.ts`: the dispatch authorizer the permission extension
//! registers, built on its live parser registry, evaluator and approval handler.
use crate::{
    dispatch::{DispatchAuthorizer, DispatchFuture, DispatchPolicy, DispatchRequest},
    parsers::{ParserRegistry, PermissionRequest},
    service::{DecisionOptions, PermissionService},
    types::Action,
};
use maho_ext_api::{ExtensionContext, ExtensionFuture, ToolCallEvent, ToolCallEventResult};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

/// senpi `DispatchPermissionState` once loaded; an unloaded or failed setup refuses in `policy_for`.
pub struct DispatchPermissionState<'a> {
    pub service: &'a PermissionService,
    pub parsers: &'a ParserRegistry,
    /// The `auto` preset is active.
    pub preset_bound: bool,
}

/// The live decision for a dispatch: deny if any pattern is denied, else ask if any pattern asks
/// (unless the request only checks denials), else allow. The fingerprint names everything the
/// decision was made from, so an approval is honored only while it is unchanged.
pub fn dispatch_policy(
    state: &DispatchPermissionState<'_>,
    request: &DispatchRequest,
    ctx: &ExtensionContext,
    home: &Path,
) -> DispatchPolicy {
    let requests = state
        .parsers
        .parse(&request.tool_name, &request.input, (&ctx.cwd, home));
    decide_dispatch_policy(state.service, state.preset_bound, &requests, ctx)
}

/// The decision half of [`dispatch_policy`] over already parsed requests, so a caller can run the
/// parser (and the tool-definition or custom-parser callbacks it invokes) without holding the lock
/// that guards the service.
pub fn decide_dispatch_policy(
    service: &PermissionService,
    preset_bound: bool,
    requests: &[PermissionRequest],
    ctx: &ExtensionContext,
) -> DispatchPolicy {
    let mut action = Action::Allow;
    let decisions: Vec<Value> = requests
        .iter()
        .map(|permission| {
            let options = DecisionOptions {
                rule_aliases: permission.rule_aliases.clone(),
                original_paths: permission.original_paths.clone(),
            };
            let rules: Vec<Value> = permission
                .patterns
                .iter()
                .map(|pattern| {
                    let decision = service.decide(&permission.permission, pattern, &options);
                    match decision.action {
                        Action::Deny => action = Action::Deny,
                        Action::Ask if !permission.auto_approve_ask && action != Action::Deny => {
                            action = Action::Ask;
                        }
                        _ => {}
                    }
                    json!({"action": decision.action, "rules": decision.rules})
                })
                .collect();
            json!({
                "permission": {
                    "permission": permission.permission,
                    "patterns": permission.patterns,
                    "always": permission.always,
                    "originalPaths": permission.original_paths,
                    "ruleAliases": permission.rule_aliases,
                    "autoApproveAsk": permission.auto_approve_ask,
                },
                "rules": rules,
            })
        })
        .collect();
    DispatchPolicy {
        action,
        fingerprint: json!([
            ctx.session_manager.session_id(),
            ctx.cwd,
            preset_bound,
            decisions
        ])
        .to_string(),
    }
}

/// The live policy for a request; an error refuses it (setup failed or not loaded).
pub type PolicyFor = Arc<
    dyn Fn(&DispatchRequest, &ExtensionContext) -> Result<DispatchPolicy, String> + Send + Sync,
>;
/// The extension's `tool_call` authorization, run as the approval prompt.
pub type AuthorizeCall = Arc<
    dyn for<'a> Fn(
            ToolCallEvent,
            &'a ExtensionContext,
        ) -> ExtensionFuture<'a, Option<ToolCallEventResult>>
        + Send
        + Sync,
>;

struct PermissionDispatchAuthorizer {
    policy_for: PolicyFor,
    authorize: AuthorizeCall,
}
impl DispatchAuthorizer for PermissionDispatchAuthorizer {
    fn policy(
        &self,
        request: &DispatchRequest,
        ctx: &ExtensionContext,
    ) -> Result<DispatchPolicy, String> {
        (self.policy_for)(request, ctx)
    }
    fn ask<'a>(
        &'a self,
        request: &'a DispatchRequest,
        ctx: &'a ExtensionContext,
        signal: maho_ext_api::AbortSignal,
    ) -> DispatchFuture<'a> {
        let event = ToolCallEvent {
            tool_call_id: request.tool_call_id.clone(),
            tool_name: request.tool_name.clone(),
            input: request.input.clone(),
            parent_tool_call_id: None,
        };
        Box::pin(async move {
            if signal.is_aborted() {
                return Err("Permission request was aborted".into());
            }
            match (self.authorize)(event, ctx).await {
                Ok(Some(result)) if result.block == Some(true) => Err(result
                    .reason
                    .unwrap_or_else(|| "Permission request was rejected.".into())),
                Ok(_) => Ok(()),
                Err(error) => Err(error.message),
            }
        })
    }
}

/// senpi `createDispatchAuthorizer`: use the permission extension's live parser, evaluator and
/// approval handler.
pub fn create_dispatch_authorizer(
    policy_for: PolicyFor,
    authorize: AuthorizeCall,
) -> Arc<dyn DispatchAuthorizer> {
    Arc::new(PermissionDispatchAuthorizer {
        policy_for,
        authorize,
    })
}
