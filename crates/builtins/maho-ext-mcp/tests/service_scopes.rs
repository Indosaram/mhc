//! Tests for S16c multi-scope routing, `skill_reveal`, `#5289` active-set ordering, and `#6485` `assert_loadable_mcp_config`.
use maho_ext_mcp::{
    catalog::McpToolCatalogEntry,
    config::assert_loadable_mcp_config,
    config_schema::McpSettings,
    expose::tier_b::{McpTierBRegistry, register_mcp_tier_b_tools},
    host_registry::HostMcpRegistry,
    request_origin::{
        LegacyMcpCallerUi, McpRequestOrigin, begin_scoped_mcp_call, create_mcp_request_origin,
        legacy_mcp_caller_ui,
    },
    service::{McpAttachOptions, McpService},
    skill_reveal::create_mcp_skill_reveal,
    skills::{RegisteredSkillTool, SkillLike},
    tool_registrar::McpToolRegistrar,
    transport_sdk::McpClient,
};
use maho_ext_tool_search::service::ToolSearchService;
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

#[test]
fn skill_reveal_is_isolated_per_execution_scope() {
    let dir = tempfile::tempdir().unwrap();
    let alpha_dir = dir.path().join("alpha");
    let beta_dir = dir.path().join("beta");
    std::fs::create_dir_all(&alpha_dir).unwrap();
    std::fs::create_dir_all(&beta_dir).unwrap();
    let alpha_md = alpha_dir.join("SKILL.md");
    let beta_md = beta_dir.join("SKILL.md");
    std::fs::write(
        &alpha_md,
        "---\nname: alpha\nmcp:\n  s:\n    command: unused\n    includeTools:\n      - t\n---\n",
    )
    .unwrap();
    std::fs::write(&beta_md, "---\nname: beta\n---\n").unwrap();

    let alpha = SkillLike {
        name: "alpha".into(),
        file_path: alpha_md,
        base_dir: alpha_dir,
        ..Default::default()
    };
    let beta = SkillLike {
        name: "beta".into(),
        file_path: beta_md,
        base_dir: beta_dir,
        ..Default::default()
    };

    let activations = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let activations_sink = activations.clone();
    let reveal = create_mcp_skill_reveal(
        Arc::new(|| {
            vec![RegisteredSkillTool {
                name: "mcp_s_t".into(),
                tool_name: "t".into(),
                server: "s".into(),
            }]
        }),
        Arc::new(move |targets| {
            activations_sink.lock().unwrap().push(targets.to_vec());
        }),
    );

    // Scope A lists alpha; scope B lists beta: B's turn does not overwrite A's skills.
    reveal.start_turn("scope-a", std::slice::from_ref(&alpha));
    reveal.start_turn("scope-b", &[beta]);
    reveal.reveal("scope-a", "alpha");
    assert_eq!(activations.lock().unwrap().len(), 1);
    assert_eq!(activations.lock().unwrap()[0], vec!["mcp_s_t"]);

    // Scope B also listing alpha has its own reveal budget.
    reveal.start_turn("scope-b", &[alpha]);
    reveal.reveal("scope-b", "alpha");
    assert_eq!(activations.lock().unwrap().len(), 2);
    // Repeated reveal in the same scope does nothing.
    reveal.reveal("scope-b", "alpha");
    assert_eq!(activations.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn service_scopes_route_origins_and_refuse_concurrent_cross_scope_legacy_elicitations() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("cwd");
    let agent_dir = dir.path().join("agent");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("mcp.json"),
        json!({
            "mcpServers": {
                "fx": {
                    "type": "stdio",
                    "command": crate::support::node(),
                    "args": [crate::support::senpi("packages/coding-agent/test/mcp/fixtures/stdio-server.ts")],
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    let registry = Arc::new(HostMcpRegistry::default());
    let mut service = McpService::new(registry, 16);
    service
        .attach_session_with_options(McpAttachOptions {
            cwd: &cwd,
            agent_dir: &agent_dir,
            env: &BTreeMap::new(),
            project_trusted: true,
            declarations: &[],
            mode: Some(maho_ext_api::ExtensionMode::Print),
            session_id: Some("scope-a"),
            ui: None,
            has_ui: true,
            extension_connects_mcp_servers: false,
        })
        .await
        .unwrap();
    service.attach_scope("scope-b", None, false).await;

    let binding = service.session_binding();
    assert!(binding.serves_scope(Some("scope-b")));
    assert!(!binding.serves_scope(Some("scope-a")));

    let conn = service.connections["fx"]
        .entry
        .lock()
        .await
        .connection
        .clone();
    let origin_a = binding.request_origin_for("fx", &conn, Some("scope-a"));
    let origin_b = binding.request_origin_for("fx", &conn, Some("scope-b"));
    assert_eq!(origin_a.scope.as_deref(), Some("scope-a"));
    assert_eq!(origin_b.scope.as_deref(), Some("scope-b"));

    // Simulate concurrent scoped calls on the physical client: legacy_mcp_caller_ui refuses.
    let client = McpClient::materialize(
        "fx",
        &maho_ext_mcp::transport_sdk::McpTransportSpec::Http {
            url: "http://127.0.0.1:9/mcp".parse().unwrap(),
            headers: BTreeMap::new(),
        },
        Arc::new(Mutex::new(
            maho_ext_mcp::log::McpLogger::new("fx", &agent_dir, None).unwrap(),
        )),
    )
    .await
    .unwrap();
    let origin_a_client = create_mcp_request_origin(
        McpRequestOrigin::new(
            1,
            conn.clone(),
            maho_ext_api::AbortSignal::default(),
            None,
            None,
            false,
            "fx".into(),
            None,
            Arc::new(|| true),
            Arc::new(|_| Box::pin(async { Ok(serde_json::Value::Null) })),
        )
        .with_scope(Some("scope-a".into()), None),
    );
    let origin_b_client = create_mcp_request_origin(
        McpRequestOrigin::new(
            1,
            conn,
            maho_ext_api::AbortSignal::default(),
            None,
            None,
            false,
            "fx".into(),
            None,
            Arc::new(|| true),
            Arc::new(|_| Box::pin(async { Ok(serde_json::Value::Null) })),
        )
        .with_scope(Some("scope-b".into()), None),
    );

    let g1 = begin_scoped_mcp_call(&client, &origin_a_client, None).unwrap();
    assert!(matches!(
        legacy_mcp_caller_ui(&client),
        LegacyMcpCallerUi::Ui(None)
    ));
    let g2 = begin_scoped_mcp_call(&client, &origin_b_client, None).unwrap();
    assert!(matches!(
        legacy_mcp_caller_ui(&client),
        LegacyMcpCallerUi::Refuse
    ));
    drop(g2);
    assert!(matches!(
        legacy_mcp_caller_ui(&client),
        LegacyMcpCallerUi::Ui(None)
    ));
    drop(g1);
    assert!(matches!(
        legacy_mcp_caller_ui(&client),
        LegacyMcpCallerUi::None
    ));

    service.dispose().await.unwrap();
}

#[derive(Default)]
struct RecordingRegistrar {
    active: Mutex<Vec<String>>,
}
impl McpToolRegistrar for RecordingRegistrar {
    fn get_active_tools(&self) -> Result<Vec<String>, maho_ext_api::ExtensionFailure> {
        Ok(self.active.lock().unwrap().clone())
    }
    fn set_active_tools(&self, names: Vec<String>) -> Result<(), maho_ext_api::ExtensionFailure> {
        *self.active.lock().unwrap() = names;
        Ok(())
    }
    fn register_tool(
        &self,
        definition: maho_ext_api::ToolDefinition,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        let mut active = self.active.lock().unwrap();
        if !active.contains(&definition.name) {
            active.push(definition.name);
        }
        Ok(())
    }
    fn register_tool_with_renderers(
        &self,
        definition: maho_ext_api::ToolDefinition,
        _renderers: maho_ext_api::ToolRenderers<(), serde_json::Value>,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        self.register_tool(definition)
    }
}

struct ScopeSessionActions(Arc<RecordingRegistrar>);
impl maho_ext_api::ExtensionSessionActions for ScopeSessionActions {
    fn set_session_name(&self, _: &str) -> Result<(), maho_ext_api::ExtensionFailure> {
        Err("unused".into())
    }
    fn get_session_name(&self) -> Result<Option<String>, maho_ext_api::ExtensionFailure> {
        Err("unused".into())
    }
    fn set_label(&self, _: &str, _: Option<&str>) -> Result<(), maho_ext_api::ExtensionFailure> {
        Err("unused".into())
    }
    fn execute_tool<'a>(
        &'a self,
        name: &'a str,
        _: serde_json::Value,
        _: maho_ext_api::ExecuteToolOptions,
    ) -> maho_ext_api::ExecuteToolFuture<'a> {
        Box::pin(async move {
            Err(maho_ext_api::ExecuteToolError {
                code: maho_ext_api::ExecuteToolErrorCode::UnknownTool,
                tool_name: name.into(),
                message: "unused".into(),
                active_tools: vec![],
            })
        })
    }
    fn get_active_tools(&self) -> Result<Vec<String>, maho_ext_api::ExtensionFailure> {
        self.0.get_active_tools()
    }
    fn set_active_tools(&self, names: Vec<String>) -> Result<(), maho_ext_api::ExtensionFailure> {
        self.0.set_active_tools(names)
    }
    fn refresh_tools(&self) -> Result<(), maho_ext_api::ExtensionFailure> {
        Ok(())
    }
    fn install_registered_tool(
        &self,
        tool: maho_ext_api::RegisteredTool,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        self.0.register_tool(tool.definition)
    }
    fn register_removed_tool_hint(
        &self,
        _: &str,
        _: &str,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        Err("unused".into())
    }
    fn register_lazy_tool_activator(
        &self,
        _: maho_ext_api::LazyToolActivator,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        Err("unused".into())
    }
    fn get_commands(
        &self,
    ) -> Result<Vec<maho_ext_api::SlashCommandInfo>, maho_ext_api::ExtensionFailure> {
        Err("unused".into())
    }
    fn set_model(&self, _: maho_ext_api::Model) -> maho_ext_api::ExtensionFuture<'_, bool> {
        Box::pin(async { Err("unused".into()) })
    }
    fn get_thinking_level(
        &self,
    ) -> Result<maho_ext_api::ThinkingLevel, maho_ext_api::ExtensionFailure> {
        Err("unused".into())
    }
    fn set_thinking_level(
        &self,
        _: maho_ext_api::ThinkingLevel,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        Err("unused".into())
    }
    fn set_session_model(&self, _: maho_ext_api::Model) -> maho_ext_api::ExtensionFuture<'_, bool> {
        Box::pin(async { Err("unused".into()) })
    }
    fn set_session_thinking_level(
        &self,
        _: maho_ext_api::ThinkingLevel,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        Err("unused".into())
    }
    fn set_session_fast_mode(&self, _: bool) -> Result<(), maho_ext_api::ExtensionFailure> {
        Err("unused".into())
    }
    fn exec<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: &'a std::path::Path,
        _: maho_ext_api::ExecOptions,
    ) -> maho_ext_api::ExtensionFuture<'a, maho_ext_api::ExecResult> {
        Box::pin(async { Err("unused".into()) })
    }
}

struct ScopeToolsActions;
impl maho_ext_api::ExtensionActions for ScopeToolsActions {
    fn send_message(
        &self,
        _message: maho_ext_api::CustomMessage,
        _options: maho_ext_api::SendMessageOptions,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        Err("unused".into())
    }
    fn send_user_message(
        &self,
        _content: maho_ext_api::UserMessageContent,
        _options: maho_ext_api::SendUserMessageOptions,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        Err("unused".into())
    }
    fn append_entry(
        &self,
        _custom_type: &str,
        _data: Option<serde_json::Value>,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        Err("unused".into())
    }
    fn get_all_tools(&self) -> Result<Vec<maho_ext_api::ToolInfo>, maho_ext_api::ExtensionFailure> {
        Ok(Vec::new())
    }
}

fn direct_order_for(order: &[&str], with_utility: bool) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let registrar = Arc::new(RecordingRegistrar {
        active: Mutex::new(vec!["read".into(), "bash".into()]),
    });
    let runtime = maho_ext_api::ExtensionRuntime::default();
    runtime.bind_session_actions(Arc::new(ScopeSessionActions(registrar.clone())));
    let mut tool_search = ToolSearchService::new(runtime, Arc::new(ScopeToolsActions));
    let mut registry = McpTierBRegistry::default();
    let mut entries = Vec::new();
    for server in order {
        entries.push(McpToolCatalogEntry {
            server: (*server).into(),
            tool: "tool_1".into(),
            schema: json!({"type": "object"}),
            output_schema: None,
            modern: false,
            description: None,
            annotations: None,
            request_timeout: Duration::from_secs(1),
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
        });
        let utility_tools = if with_utility {
            vec![maho_ext_api::ToolDefinition::new(
                "mcp_list_resources",
                "list",
                json!({"type": "object"}),
                Arc::new(|_| Box::pin(async { Ok(maho_ext_api::ToolResult::text("ok")) })),
            )]
        } else {
            Vec::new()
        };
        register_mcp_tier_b_tools(
            registrar.clone(),
            maho_ext_mcp::expose::tier_b::McpTierBRegistrationInput {
                registered_entries: entries.clone(),
                active_entries: entries.clone(),
                search_mode: false,
                proxy_gateways: Vec::new(),
                utility_tools,
                settings: McpSettings::default(),
                agent_dir: dir.path().to_path_buf(),
                artifacts: Default::default(),
                output_guard: None,
            },
            Some(&mut tool_search),
            &mut registry,
            None,
        )
        .unwrap();
    }
    registrar.get_active_tools().unwrap()
}

#[test]
fn direct_mode_tools_keep_deterministic_order_regardless_of_connect_order() {
    let http_first = direct_order_for(&["fxhttp", "fxstdio"], false);
    let stdio_first = direct_order_for(&["fxstdio", "fxhttp"], false);
    assert_eq!(stdio_first, http_first);
    assert_eq!(
        http_first,
        vec!["read", "bash", "mcp_fxhttp_tool_1", "mcp_fxstdio_tool_1"]
    );

    let with_util_http = direct_order_for(&["fxhttp", "fxstdio"], true);
    let with_util_stdio = direct_order_for(&["fxstdio", "fxhttp"], true);
    assert_eq!(with_util_stdio, with_util_http);
    assert_eq!(
        with_util_http,
        vec![
            "read",
            "bash",
            "mcp_list_resources",
            "mcp_fxhttp_tool_1",
            "mcp_fxstdio_tool_1"
        ]
    );
}

#[test]
fn assert_loadable_mcp_config_validates_without_side_effects() {
    let valid = json!({"mcpServers": {"ok": {"command": "echo", "args": ["hi"]}}});
    assert!(assert_loadable_mcp_config(valid, &BTreeMap::new()).is_ok());

    let invalid = json!({"mcpServers": {"bad": {"type": "http"}}});
    assert!(assert_loadable_mcp_config(invalid, &BTreeMap::new()).is_err());
}
