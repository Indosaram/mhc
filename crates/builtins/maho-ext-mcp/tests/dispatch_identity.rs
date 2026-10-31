//! ext-mcp rows 24, 25, 27 and 57: offered MCP definitions name the operation they dispatch, and a call is
//! resolved against the live catalog and the permission dispatch fence before it is sent.
use maho_ext_api::{
    AbortSignal, BuildSystemPromptOptions, ComponentFactory, CustomUiOptions, ExtensionContext,
    ExtensionFuture, ExtensionMode, ExtensionUi, ExtensionUiDialogOptions, ExtensionWidgetOptions,
    JsonValue, Model, ModelRegistry, NotificationType, SessionEntry, SessionManager, Theme,
    ToolDefinition, ToolDispatchOwner, ToolSessionManager, UiFuture, WidgetContent,
    invocation_input::InvocationInput,
};
use maho_ext_mcp::{
    catalog::{McpCatalogCallback, McpToolCatalogEntry},
    expose::{
        proxy::create_mcp_proxy_tool,
        register::build_mcp_tool_definitions,
        tier_b::{McpStubPromotion, build_mcp_stub_definition},
    },
    guard::output_guard::McpOutputArtifacts,
    invocation::{
        McpInvocation, McpInvocationErrorKind, McpInvocationResolver, McpInvocationScope,
        McpPreparedInvocation, McpToolLookup, create_mcp_invocation_resolver,
    },
};
use maho_ext_permission_system::{
    dispatch::{
        DispatchAuthorizer, DispatchFuture, DispatchPolicy, DispatchRequest,
        register_dispatch_authorizer,
    },
    dispatch_metadata::get_dispatch_identity,
    types::Action,
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Session(String);
impl ToolSessionManager for Session {
    fn session_id(&self) -> &str {
        &self.0
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}
impl SessionManager for Session {
    fn get_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_branch(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_leaf_id(&self) -> Option<String> {
        None
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
}
struct Registry;
impl ModelRegistry for Registry {
    fn get_all(&self) -> Vec<Model> {
        Vec::new()
    }
    fn get_available(&self) -> Vec<Model> {
        Vec::new()
    }
    fn find(&self, _: &str, _: &str) -> Option<Model> {
        None
    }
    fn has_configured_auth(&self, _: &Model) -> bool {
        false
    }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}
struct Ui;
impl ExtensionUi for Ui {
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err("UI not available".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}
fn context(session: &Arc<dyn SessionManager>, loaded: &[&str]) -> ExtensionContext {
    ExtensionContext {
        ui: Arc::new(Ui),
        mode: ExtensionMode::Print,
        has_ui: false,
        cwd: "/tmp".into(),
        agent_dir: "/tmp/agent".into(),
        session_manager: Arc::clone(session),
        model_registry: Arc::new(Registry),
        model: None,
        thinking_level: None,
        service_tier: None,
        effective_service_tier: None,
        scoped_models: Vec::new(),
        goal_store_file: None,
        loaded_extension_paths: loaded.iter().map(|path| (*path).to_owned()).collect(),
        signal: None,
        steering_signal: None,
        is_idle_fn: Arc::new(|| true),
        wait_for_idle_fn: Arc::new(|| Box::pin(async {})),
        is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false),
        get_system_prompt_fn: Arc::new(String::new),
        get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        idle_coordinator: None,
        logger: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

fn entry(tool: &str, schema: Value) -> McpToolCatalogEntry {
    McpToolCatalogEntry {
        server: "fx".into(),
        tool: tool.into(),
        schema,
        output_schema: None,
        modern: false,
        description: Some(format!("{tool} tool")),
        annotations: None,
        request_timeout: Duration::from_secs(30),
        client: None,
        runtime: None,
        connection: None,
        ensure_connected: None,
        ensure_fresh: None,
        agent_dir: None,
        artifacts: None,
        output_guard: None,
        request_origin: None,
        invocation: None,
    }
}

/// A resolver over an in-memory catalog the test mutates.
struct Fixture {
    resolver: Arc<McpInvocationResolver>,
    catalog: Arc<Mutex<Vec<McpToolCatalogEntry>>>,
    current: Arc<AtomicBool>,
    readies: Arc<AtomicUsize>,
    tools: Arc<Mutex<Vec<ToolDefinition>>>,
    owner: ToolDispatchOwner,
}
fn fixture(configuration: &str) -> Fixture {
    let catalog = Arc::new(Mutex::new(vec![entry(
        "write",
        json!({"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}),
    )]));
    let current = Arc::new(AtomicBool::new(true));
    let readies = Arc::new(AtomicUsize::new(0));
    let tools: Arc<Mutex<Vec<ToolDefinition>>> = Arc::default();
    let owner = ToolDispatchOwner::new();
    let find = {
        let catalog = Arc::clone(&catalog);
        move |tool: &str| {
            catalog
                .lock()
                .expect("catalog")
                .iter()
                .find(|entry| entry.tool == tool)
                .cloned()
        }
    };
    let ready: McpCatalogCallback = {
        let readies = Arc::clone(&readies);
        Arc::new(move || {
            readies.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok::<(), maho_ext_mcp::errors::McpError>(()) })
        })
    };
    let lookup: McpToolLookup = {
        let tools = Arc::clone(&tools);
        Arc::new(move |name: &str| {
            tools
                .lock()
                .expect("tools")
                .iter()
                .find(|tool| tool.name == name)
                .cloned()
        })
    };
    let async_find = find.clone();
    let resolver = Arc::new(create_mcp_invocation_resolver(McpInvocationScope {
        owner: owner.clone(),
        configuration: configuration.into(),
        context_required: false,
        session_id: None,
        is_current: {
            let current = Arc::clone(&current);
            Arc::new(move || current.load(Ordering::SeqCst))
        },
        ready,
        current: Arc::new(move |tool: String| {
            let found = async_find(&tool);
            Box::pin(async move { found })
        }),
        current_now: Arc::new(find),
        lookup: Some(lookup),
    }));
    Fixture {
        resolver,
        catalog,
        current,
        readies,
        tools,
        owner,
    }
}
impl Fixture {
    /// The offered entry carrying this resolver.
    fn offered(&self) -> McpToolCatalogEntry {
        let mut offered = self.catalog.lock().expect("catalog")[0].clone();
        offered.invocation = Some(Arc::clone(&self.resolver));
        offered
    }
    /// Register the direct definitions built from the offered catalog as the live tools.
    fn publish(&self) -> Vec<ToolDefinition> {
        let definitions = build_mcp_tool_definitions(
            &[self.offered()],
            std::env::temp_dir(),
            Arc::new(McpOutputArtifacts::default()),
            None,
        );
        *self.tools.lock().expect("tools") = definitions.clone();
        definitions
    }
}

struct Authorizer {
    action: Action,
    asks: AtomicUsize,
}
impl DispatchAuthorizer for Authorizer {
    fn policy(&self, _: &DispatchRequest, _: &ExtensionContext) -> Result<DispatchPolicy, String> {
        Ok(DispatchPolicy {
            action: self.action,
            fingerprint: format!("{:?}", self.action),
        })
    }
    fn ask<'a>(
        &'a self,
        _: &'a DispatchRequest,
        _: &'a ExtensionContext,
        _: AbortSignal,
    ) -> DispatchFuture<'a> {
        self.asks.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err("The user rejected this MCP call.".to_owned()) })
    }
}

fn invocation<'a>(name: &str, input: Value, ctx: &'a ExtensionContext) -> McpInvocation<'a> {
    McpInvocation {
        tool_call_id: "call-1".into(),
        tool_name: name.into(),
        input,
        input_identity: Some(InvocationInput::new()),
        context: Some(ctx),
    }
}

#[test]
fn direct_definitions_name_the_resolver_identity_of_their_entry() {
    let fx = fixture("config-a");
    let definitions = fx.publish();
    let identity = get_dispatch_identity(&definitions[0], &json!({"value":"x"})).expect("identity");
    assert_eq!(identity.owner, fx.owner);
    assert_eq!(identity.operation.as_deref(), Some(r#"["fx","write"]"#));
    assert!(
        identity
            .metadata
            .starts_with(r#"["config-a","fx","write","#)
    );
    // An entry built outside a session registration names no operation.
    let plain = build_mcp_tool_definitions(
        &[entry("write", json!({"type":"object"}))],
        std::env::temp_dir(),
        Arc::new(McpOutputArtifacts::default()),
        None,
    );
    assert!(get_dispatch_identity(&plain[0], &json!({})).is_none());
}

#[test]
fn proxy_gateway_names_the_operation_only_for_a_call_of_a_known_tool() {
    let fx = fixture("config-a");
    let gateway = create_mcp_proxy_tool(
        "fx",
        &[fx.offered()],
        std::env::temp_dir(),
        Arc::new(McpOutputArtifacts::default()),
        None,
    );
    let call = get_dispatch_identity(&gateway, &json!({"op":"call","tool":"write","args":"{}"}))
        .expect("call identity");
    assert_eq!(call.owner, fx.owner);
    assert_eq!(call.operation.as_deref(), Some(r#"["fx","write"]"#));
    for input in [
        json!({"op":"search","query":"write"}),
        json!({"op":"describe","tool":"write"}),
        json!({"op":"call","tool":"missing"}),
        json!({"op":"call"}),
    ] {
        assert!(get_dispatch_identity(&gateway, &input).is_none(), "{input}");
    }
}

#[test]
fn deferred_stub_forwards_the_promoted_definition_identity() {
    let fx = fixture("config-a");
    let full = fx.publish().remove(0);
    let expected = get_dispatch_identity(&full, &json!({"value":"x"})).expect("full identity");
    let name = full.name.clone();
    let stub = build_mcp_stub_definition(
        &name,
        Some(McpStubPromotion {
            full,
            promote: Arc::new(|| Ok(())),
        }),
    );
    assert_eq!(
        get_dispatch_identity(&stub, &json!({"value":"x"})),
        Some(expected)
    );
    assert!(
        get_dispatch_identity(&build_mcp_stub_definition("mcp_fx_write", None), &json!({}))
            .is_none()
    );
}

#[tokio::test]
async fn resolve_allows_and_fences_on_the_live_permission_policy() {
    let fx = fixture("config-a");
    let definitions = fx.publish();
    let session: Arc<dyn SessionManager> = Arc::new(Session("mcp-allow".into()));
    let ctx = context(&session, &[]);
    let authorizer = Arc::new(Authorizer {
        action: Action::Allow,
        asks: AtomicUsize::new(0),
    });
    let retirement = register_dispatch_authorizer(&session, authorizer.clone());
    let args = json!({"value":"x"});
    let call = invocation(&definitions[0].name, args.clone(), &ctx);
    let McpPreparedInvocation::Ready(ready) = fx
        .resolver
        .resolve(&fx.offered(), &args, Some(&call), None)
        .await
        .expect("resolved")
    else {
        panic!("refused");
    };
    assert_eq!(fx.readies.load(Ordering::SeqCst), 1);
    assert!(ready.is_current());
    // A retired authorizer closes the fence before the send.
    retirement.retire();
    assert!(!ready.is_current());
    assert_eq!(authorizer.asks.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn resolve_refuses_a_denied_or_rejected_operation() {
    let fx = fixture("config-a");
    let definitions = fx.publish();
    let args = json!({"value":"x"});
    for (id, action, asked) in [("mcp-deny", Action::Deny, 0), ("mcp-ask", Action::Ask, 1)] {
        let session: Arc<dyn SessionManager> = Arc::new(Session(id.into()));
        let ctx = context(&session, &[]);
        let authorizer = Arc::new(Authorizer {
            action,
            asks: AtomicUsize::new(0),
        });
        let _retirement = register_dispatch_authorizer(&session, authorizer.clone());
        let call = invocation(&definitions[0].name, args.clone(), &ctx);
        let McpPreparedInvocation::Refused(error) = fx
            .resolver
            .resolve(&fx.offered(), &args, Some(&call), None)
            .await
            .expect("resolved")
        else {
            panic!("{id} was not refused");
        };
        assert_eq!(error.kind, McpInvocationErrorKind::PermissionDenied, "{id}");
        assert_eq!(authorizer.asks.load(Ordering::SeqCst), asked, "{id}");
    }
}

#[tokio::test]
async fn resolve_refuses_when_the_permission_system_is_loaded_without_an_authorizer() {
    let fx = fixture("config-a");
    let definitions = fx.publish();
    let session: Arc<dyn SessionManager> = Arc::new(Session("mcp-unregistered".into()));
    let ctx = context(&session, &["<builtin:permission-system>"]);
    let args = json!({"value":"x"});
    let call = invocation(&definitions[0].name, args.clone(), &ctx);
    let McpPreparedInvocation::Refused(error) = fx
        .resolver
        .resolve(&fx.offered(), &args, Some(&call), None)
        .await
        .expect("resolved")
    else {
        panic!("not refused");
    };
    assert_eq!(
        (error.kind, error.message.as_str()),
        (
            McpInvocationErrorKind::PermissionDenied,
            "Permission authorizer is unavailable."
        )
    );
    // Without the permission system loaded the call proceeds unfenced.
    let ctx = context(&session, &[]);
    let call = invocation(&definitions[0].name, args.clone(), &ctx);
    assert!(matches!(
        fx.resolver
            .resolve(&fx.offered(), &args, Some(&call), None)
            .await
            .expect("resolved"),
        McpPreparedInvocation::Ready(_)
    ));
}

#[tokio::test]
async fn resolve_refuses_stale_sessions_schemas_and_operation_names() {
    let fx = fixture("config-a");
    let definitions = fx.publish();
    let session: Arc<dyn SessionManager> = Arc::new(Session("mcp-stale".into()));
    let ctx = context(&session, &[]);
    let refusal = |prepared: McpPreparedInvocation| match prepared {
        McpPreparedInvocation::Refused(error) => (error.kind, error.message),
        McpPreparedInvocation::Ready(_) => panic!("not refused"),
    };
    // Arguments that the current raw schema rejects.
    let bad = json!({"value": 1});
    let call = invocation(&definitions[0].name, bad.clone(), &ctx);
    assert_eq!(
        refusal(
            fx.resolver
                .resolve(&fx.offered(), &bad, Some(&call), None)
                .await
                .expect("resolved")
        ),
        (
            McpInvocationErrorKind::SchemaChanged,
            "Arguments do not match the current MCP tool schema.".into()
        )
    );
    // A tool name whose live definition no longer dispatches this operation.
    let args = json!({"value":"x"});
    let call = invocation("mcp_other_tool", args.clone(), &ctx);
    assert_eq!(
        refusal(
            fx.resolver
                .resolve(&fx.offered(), &args, Some(&call), None)
                .await
                .expect("resolved")
        ),
        (
            McpInvocationErrorKind::Unavailable,
            "MCP invocation name no longer identifies this operation.".into()
        )
    );
    // A replaced binding or configuration.
    fx.current.store(false, Ordering::SeqCst);
    let call = invocation(&definitions[0].name, args.clone(), &ctx);
    assert_eq!(
        refusal(
            fx.resolver
                .resolve(&fx.offered(), &args, Some(&call), None)
                .await
                .expect("resolved")
        )
        .0,
        McpInvocationErrorKind::Unavailable
    );
}

#[tokio::test]
async fn a_changed_catalog_entry_closes_the_fence_before_the_send() {
    let fx = fixture("config-a");
    let definitions = fx.publish();
    let session: Arc<dyn SessionManager> = Arc::new(Session("mcp-changed".into()));
    let ctx = context(&session, &[]);
    let args = json!({"value":"x"});
    let call = invocation(&definitions[0].name, args.clone(), &ctx);
    let McpPreparedInvocation::Ready(ready) = fx
        .resolver
        .resolve(&fx.offered(), &args, Some(&call), None)
        .await
        .expect("resolved")
    else {
        panic!("refused");
    };
    assert!(ready.is_current());
    fx.catalog.lock().expect("catalog")[0].description = Some("changed".into());
    assert!(!ready.is_current());
    fx.catalog.lock().expect("catalog").clear();
    assert!(!ready.is_current());
}

#[tokio::test]
async fn an_aborted_call_is_an_error_not_a_refusal() {
    let fx = fixture("config-a");
    let signal = AbortSignal::default();
    signal.abort();
    let result = fx
        .resolver
        .resolve(&fx.offered(), &json!({"value":"x"}), None, Some(&signal))
        .await;
    assert!(result.is_err());
    assert_eq!(fx.readies.load(Ordering::SeqCst), 0);
}
