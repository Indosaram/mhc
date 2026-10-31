//! dispatch-policy.ts: the live dispatch decision the permission extension serves to dispatching
//! tools, and its registration over the session lifecycle.
use crate::context_support::{TestSession, context};
use maho_ext_api::{
    EventBus, EventKind, Extension, ExtensionApi, ExtensionEvent, ExtensionFailure,
    ExtensionRuntime, ExtensionSessionActions, ExtensionSessionProfile, FlagValue, JsonValue,
    LoadedExtension, Model, SessionManager, SourceInfo, ToolDispatchIdentity, ToolDispatchOwner,
    invocation_input::InvocationInput,
};
use maho_ext_permission_system::{
    PermissionSystem,
    dispatch::{DispatchError, DispatchRequest, authorize_tool_dispatch, has_dispatch_authorizer},
    dispatch_policy::{DispatchPermissionState, dispatch_policy},
    events::PermissionEventEmitter,
    parsers::create_builtin_parser_registry,
    service::PermissionService,
    types::{Action, Rule},
};
use serde_json::json;
use std::{path::Path, sync::Arc};

fn rule(permission: &str, pattern: &str, action: Action) -> Rule {
    Rule {
        permission: permission.into(),
        pattern: pattern.into(),
        action,
    }
}
fn request(tool: &str, input: serde_json::Value) -> DispatchRequest {
    DispatchRequest {
        tool_call_id: "call".into(),
        parent_tool_call_id: None,
        tool_name: tool.into(),
        input,
        input_identity: InvocationInput::new(),
        identity: ToolDispatchIdentity {
            owner: ToolDispatchOwner::new(),
            metadata: "op".into(),
            operation: None,
        },
    }
}

#[test]
fn policy_is_the_strictest_parsed_decision_and_fingerprints_its_inputs() {
    let cwd = tempfile::tempdir().expect("cwd");
    let session: Arc<dyn SessionManager> = Arc::new(TestSession("policy".into()));
    let ctx = context(session, cwd.path());
    let parsers = create_builtin_parser_registry();
    let home = Path::new("/home/user");
    let decide = |rules: Vec<Rule>, tool: &str, input: serde_json::Value| {
        let service = PermissionService::new(rules, vec![], PermissionEventEmitter::default());
        dispatch_policy(
            &DispatchPermissionState {
                service: &service,
                parsers: &parsers,
                preset_bound: false,
            },
            &request(tool, input),
            &ctx,
            home,
        )
    };
    let fixture = json!({"value":"payload"});
    let allow = decide(
        vec![rule("*", "*", Action::Allow)],
        "mcp_fixture_write",
        fixture.clone(),
    );
    let ask = decide(
        vec![rule("*", "*", Action::Ask)],
        "mcp_fixture_write",
        fixture.clone(),
    );
    let deny = decide(
        vec![
            rule("*", "*", Action::Allow),
            rule("mcp_fixture_write", "*", Action::Deny),
        ],
        "mcp_fixture_write",
        fixture.clone(),
    );
    assert_eq!(
        (allow.action, ask.action, deny.action),
        (Action::Allow, Action::Ask, Action::Deny)
    );
    // Any denied command in a compound bash call denies the dispatch over an earlier ask.
    let compound = decide(
        vec![
            rule("bash", "*", Action::Ask),
            rule("bash", "rm *", Action::Deny),
        ],
        "bash",
        json!({"command":"ls; rm -rf build"}),
    );
    assert_eq!(compound.action, Action::Deny);
    // The fingerprint changes with the rules that decided it, so an approval cannot outlive them.
    let again = decide(
        vec![rule("*", "*", Action::Ask)],
        "mcp_fixture_write",
        fixture.clone(),
    );
    let narrowed = decide(
        vec![
            rule("*", "*", Action::Ask),
            rule("mcp_fixture_write", "*", Action::Ask),
        ],
        "mcp_fixture_write",
        fixture,
    );
    assert_eq!(ask.fingerprint, again.fingerprint);
    assert_ne!(ask.fingerprint, narrowed.fingerprint);
}

struct Actions;
impl ExtensionSessionActions for Actions {
    fn set_session_name(&self, _: &str) -> Result<(), ExtensionFailure> {
        unreachable!()
    }
    fn get_session_name(&self) -> Result<Option<String>, ExtensionFailure> {
        unreachable!()
    }
    fn set_label(&self, _: &str, _: Option<&str>) -> Result<(), ExtensionFailure> {
        unreachable!()
    }
    fn execute_tool<'a>(
        &'a self,
        _: &'a str,
        _: JsonValue,
        _: maho_ext_api::ExecuteToolOptions,
    ) -> maho_ext_api::ExecuteToolFuture<'a> {
        unreachable!()
    }
    fn refresh_tools(&self) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn register_removed_tool_hint(&self, _: &str, _: &str) -> Result<(), ExtensionFailure> {
        unreachable!()
    }
    fn register_lazy_tool_activator(
        &self,
        _: maho_ext_api::LazyToolActivator,
    ) -> Result<(), ExtensionFailure> {
        unreachable!()
    }
    fn get_commands(&self) -> Result<Vec<maho_ext_api::SlashCommandInfo>, ExtensionFailure> {
        unreachable!()
    }
    fn set_model(&self, _: Model) -> maho_ext_api::ExtensionFuture<'_, bool> {
        unreachable!()
    }
    fn get_thinking_level(&self) -> Result<maho_ext_api::ThinkingLevel, ExtensionFailure> {
        unreachable!()
    }
    fn set_thinking_level(&self, _: maho_ext_api::ThinkingLevel) -> Result<(), ExtensionFailure> {
        unreachable!()
    }
    fn set_session_model(&self, _: Model) -> maho_ext_api::ExtensionFuture<'_, bool> {
        unreachable!()
    }
    fn set_session_thinking_level(
        &self,
        _: maho_ext_api::ThinkingLevel,
    ) -> Result<(), ExtensionFailure> {
        unreachable!()
    }
    fn set_session_fast_mode(&self, _: bool) -> Result<(), ExtensionFailure> {
        unreachable!()
    }
    fn exec<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: &'a Path,
        _: maho_ext_api::ExecOptions,
    ) -> maho_ext_api::ExtensionFuture<'a, maho_ext_api::ExecResult> {
        unreachable!()
    }
    fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure> {
        Ok(vec!["mcp_fixture_write".into()])
    }
    fn set_active_tools(&self, _: Vec<String>) -> Result<(), ExtensionFailure> {
        Ok(())
    }
}

#[tokio::test]
async fn extension_serves_live_dispatch_policy_until_shutdown() {
    let project = tempfile::tempdir().expect("project");
    let session: Arc<dyn SessionManager> = Arc::new(TestSession("lifecycle".into()));
    let ctx = context(Arc::clone(&session), project.path());
    let runtime = ExtensionRuntime::default();
    runtime.bind_session_actions(Arc::new(Actions));
    let scoped = runtime.registration_scope();
    let mut api = ExtensionApi::new(
        LoadedExtension::new("permission", project.path().into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        scoped.clone(),
    );
    PermissionSystem.register(&mut api);
    scoped.set_flag("permission-preset", FlagValue::String("ask".into()));
    scoped.commit_registration().expect("commit");
    assert!(!has_dispatch_authorizer(&session));

    let emit = |kind: EventKind, mut event: ExtensionEvent| {
        let handlers = api.registered.handlers[&kind].clone();
        let ctx = ctx.clone();
        async move {
            for handler in handlers {
                handler(&mut event, &ctx).await.expect("handler");
            }
        }
    };
    emit(
        EventKind::SessionStart,
        ExtensionEvent::SessionStart(maho_ext_api::SessionStartEvent {
            reason: maho_ext_api::SessionReason::Startup,
            initial_model_provenance: None,
            previous_session_file: None,
            attended: None,
        }),
    )
    .await;
    assert!(has_dispatch_authorizer(&session));

    // Under `ask` with no UI the approval prompt refuses: the dispatch is blocked with the reason.
    let call = request("mcp_fixture_write", json!({"value":"payload"}));
    let refused = authorize_tool_dispatch(&session, &call, &ctx, None)
        .await
        .expect_err("no UI to approve");
    assert!(
        matches!(&refused, DispatchError::Failed(reason) if reason.contains("Permission required for mcp_fixture_write (*), and there is no UI to ask")),
        "{refused:?}"
    );

    emit(
        EventKind::SessionShutdown,
        ExtensionEvent::SessionShutdown(maho_ext_api::SessionShutdownEvent {
            reason: maho_ext_api::SessionReason::Reload,
            target_session_file: None,
            signal: None,
        }),
    )
    .await;
    assert_eq!(
        authorize_tool_dispatch(&session, &call, &ctx, None)
            .await
            .expect_err("retired"),
        DispatchError::Retired
    );
}
