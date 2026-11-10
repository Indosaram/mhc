//! dispatch-metadata.ts: identities are scoped to the offered definition, only `mcp_` calls with a
//! registered identity get a dispatch approval, and the approval rides the call's input identity.
use crate::context_support::{TestSession, context};
use maho_ext_api::{
    AbortSignal, ExtensionContext, SessionManager, ToolCallEvent, ToolDefinition,
    ToolDispatchIdentity, ToolDispatchOwner, ToolResult,
    invocation_input::{InvocationInput, scope_invocation_input},
};
use maho_ext_permission_system::{
    dispatch::{
        DispatchAuthorizer, DispatchFuture, DispatchPolicy, DispatchRequest,
        authorize_tool_dispatch, register_dispatch_authorizer,
    },
    dispatch_metadata::{
        get_dispatch_identity, prepare_mcp_dispatch_approval, register_dispatch_identity,
    },
    types::Action,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct AskOnce(AtomicUsize);
impl DispatchAuthorizer for AskOnce {
    fn policy(&self, _: &DispatchRequest, _: &ExtensionContext) -> Result<DispatchPolicy, String> {
        Ok(DispatchPolicy {
            action: Action::Ask,
            fingerprint: "ask".into(),
        })
    }
    fn ask<'a>(
        &'a self,
        _: &'a DispatchRequest,
        _: &'a ExtensionContext,
        _: AbortSignal,
    ) -> DispatchFuture<'a> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

fn definition(name: &str, owner: Option<ToolDispatchOwner>) -> ToolDefinition {
    let mut definition = ToolDefinition::new(
        name,
        "fixture",
        json!({"type":"object"}),
        Arc::new(|_| Box::pin(async { Ok(ToolResult::text("ok")) })),
    );
    if let Some(owner) = owner {
        register_dispatch_identity(&mut definition, move |input: &Value| {
            Some(ToolDispatchIdentity {
                owner: owner.clone(),
                metadata: format!("op:{}", input["value"]),
                operation: Some("write".into()),
            })
        });
    }
    definition
}
fn event(name: &str) -> ToolCallEvent {
    ToolCallEvent {
        tool_call_id: "call".into(),
        tool_name: name.into(),
        input: json!({"value":"payload"}),
        parent_tool_call_id: None,
    }
}

#[test]
fn identity_is_scoped_to_the_offered_definition() {
    let owner = ToolDispatchOwner::new();
    let offered = definition("mcp_fixture_write", Some(owner.clone()));
    let identity = get_dispatch_identity(&offered, &json!({"value":1})).expect("identity");
    assert_eq!(identity.owner, owner);
    assert_eq!(identity.metadata, "op:1");
    assert!(get_dispatch_identity(&definition("mcp_fixture_write", None), &json!({})).is_none());
}

#[tokio::test]
async fn preflight_records_an_approval_only_for_identified_mcp_calls() {
    let cwd = tempfile::tempdir().expect("cwd");
    let session: Arc<dyn SessionManager> = Arc::new(TestSession("metadata".into()));
    let ctx = context(Arc::clone(&session), cwd.path());
    let authorizer = Arc::new(AskOnce(AtomicUsize::new(0)));
    let _registration = register_dispatch_authorizer(&session, authorizer.clone());
    let owner = ToolDispatchOwner::new();
    let lookup = |name: &str| match name {
        "mcp_fixture_write" => Some(definition(name, Some(owner.clone()))),
        "mcp_plain" => Some(definition(name, None)),
        _ => None,
    };

    // Not mcp_, no identity, unknown tool, or no scoped call identity: the plain preflight path.
    let input = InvocationInput::new();
    for name in ["write", "mcp_plain", "mcp_missing"] {
        let prepared = scope_invocation_input(input.clone(), async {
            prepare_mcp_dispatch_approval(lookup, &event(name), &ctx, authorizer.as_ref())
        })
        .await;
        assert!(prepared.expect("no error").is_none(), "{name}");
    }
    assert!(
        prepare_mcp_dispatch_approval(
            lookup,
            &event("mcp_fixture_write"),
            &ctx,
            authorizer.as_ref()
        )
        .expect("no error")
        .is_none()
    );

    // An identified mcp_ call approved in preflight dispatches without asking again.
    let call = event("mcp_fixture_write");
    scope_invocation_input(input.clone(), async {
        prepare_mcp_dispatch_approval(lookup, &call, &ctx, authorizer.as_ref())
            .expect("no error")
            .expect("prepared")
            .approve()
            .expect("approved");
    })
    .await;
    let request = DispatchRequest {
        tool_call_id: call.tool_call_id.clone(),
        parent_tool_call_id: None,
        tool_name: call.tool_name.clone(),
        input: call.input.clone(),
        input_identity: input,
        identity: get_dispatch_identity(&lookup(&call.tool_name).expect("offered"), &call.input)
            .expect("identity"),
    };
    let fence = authorize_tool_dispatch(&session, &request, &ctx, None)
        .await
        .expect("preflight approval honored");
    assert!(fence.is_current(&request));
    assert_eq!(authorizer.0.load(Ordering::SeqCst), 0);
}
