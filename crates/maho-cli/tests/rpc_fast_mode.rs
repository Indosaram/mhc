use maho_core::{
    model_runtime::{CreateModelRuntimeOptions, ModelRuntime},
    sdk::{create_agent_session, CreateAgentSessionOptions, NoToolsMode},
    session_manager::SessionManager,
    settings_manager::{FileSettingsStorage, SettingsManager},
};
use maho_ext_builtin_loose::service_tier::{ServiceTierExtension, ServiceTierHost, CODEX_RESPONSES_API};
use maho_ext_host::loader::NativeAsyncExtensionFactory;
use maho_rpc::connection_handler::handle_input_line;
use maho_ext_api::{Extension, ExtensionContext, ExtensionFailure, ExtensionFuture, Model, ServiceTier};
use serde_json::{json, Value};
use std::{sync::Arc, path::Path};

struct FileTierHost;

impl ServiceTierHost for FileTierHost {
    fn catalog_tier(&self, model: &Model) -> Option<ServiceTier> {
        model.service_tier.map(|tier| match tier {
            maho_ai::types::ServiceTierPreference::Auto => ServiceTier::Auto,
            maho_ai::types::ServiceTierPreference::Flex => ServiceTier::Flex,
            maho_ai::types::ServiceTierPreference::Priority => ServiceTier::Priority, maho_ai::types::ServiceTierPreference::Ultrafast => ServiceTier::Ultrafast,
        })
    }

    fn upstream_id(&self, model: &Model) -> String {
        model.upstream_model_id.clone().unwrap_or_else(|| model.id.clone())
    }

    fn remembered(&self, ctx: &ExtensionContext, model: &Model) -> Result<Option<ServiceTier>, ExtensionFailure> {
        let manager = SettingsManager::create(
            &ctx.cwd.to_string_lossy(), &ctx.agent_dir.to_string_lossy(), "/unused-home", false,
        );
        Ok(manager.get_model_service_tier(&model.provider, &model.id).and_then(parse_tier))
    }

    fn global_tier(&self, ctx: &ExtensionContext) -> Result<Option<ServiceTier>, ExtensionFailure> {
        let manager = SettingsManager::create(
            &ctx.cwd.to_string_lossy(), &ctx.agent_dir.to_string_lossy(), "/unused-home", false,
        );
        Ok(manager.get_string("openai.serviceTier").as_deref().and_then(parse_tier))
    }

    fn persist<'a>(&'a self, ctx: &'a ExtensionContext, model: &'a Model, tier: ServiceTier) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let mut manager = SettingsManager::create(
                &ctx.cwd.to_string_lossy(), &ctx.agent_dir.to_string_lossy(), "/unused-home", false,
            );
            let tier = match tier { ServiceTier::Auto => "auto", ServiceTier::Flex => "flex", ServiceTier::Priority => "priority", ServiceTier::Ultrafast => "ultrafast" };
            manager.set_model_service_tier(&model.provider, &model.id, tier).map_err(ExtensionFailure::new)
        })
    }
}

fn parse_tier(value: &str) -> Option<ServiceTier> {
    match value { "auto" => Some(ServiceTier::Auto), "flex" => Some(ServiceTier::Flex), "priority" => Some(ServiceTier::Priority), "ultrafast" => Some(ServiceTier::Ultrafast), _ => None }
}

fn model_catalog(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let catalog = json!({"providers":{"chatgpt-subscription":{"api":CODEX_RESPONSES_API,"baseUrl":"http://localhost:0","apiKey":"fixture-key","models":[
        {"id":"codex","name":"Codex","reasoning":true,"input":["text"],"contextWindow":128000,"maxTokens":100},
        {"id":"codex-fast","name":"Codex Fast","reasoning":true,"input":["text"],"contextWindow":128000,"maxTokens":100,"serviceTier":"priority","upstreamModelId":"codex"}
    ]},"faux":{"api":"faux","baseUrl":"http://localhost:0","apiKey":"fixture-key","models":[{"id":"faux-1"}]}}});
    std::fs::write(path, serde_json::to_vec(&catalog)?)?;
    Ok(())
}

async fn rpc(session: &maho_core::agent_session::AgentSession, request: Value) -> Value {
    serde_json::from_str(&handle_input_line(session, &request.to_string()).await.expect("RPC dispatch").expect("RPC response")).expect("JSON response")
}

#[tokio::test]
async fn rpc_fast_mode_switches_persists_and_refuses_unsupported_models() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let cwd = temp.path().to_string_lossy().into_owned();
    let agent_dir = temp.path().join("agent");
    std::fs::create_dir_all(&agent_dir)?;
    let models_path = agent_dir.join("models.json");
    model_catalog(&models_path)?;
    let provider = maho_ai::providers::faux::faux_provider(maho_ai::providers::faux::RegisterFauxProviderOptions {
        api: Some(CODEX_RESPONSES_API.into()), provider: Some("chatgpt-subscription".into()),
        models: Some(["codex", "codex-fast"].into_iter().map(|id| maho_ai::providers::faux::FauxModelDefinition { id: id.into(), reasoning: Some(true), context_window:Some(128000), ..Default::default() }).collect()),
        ..Default::default()
    }).expect("valid faux provider");
    let unsupported_provider = maho_ai::providers::faux::faux_provider(maho_ai::providers::faux::RegisterFauxProviderOptions::default()).expect("valid faux provider");
    let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
        models_path: Some(models_path), auth_path: Some(agent_dir.join("auth.json")),
        providers: Some(vec![provider.provider, unsupported_provider.provider]), ..Default::default()
    }).expect("valid model runtime");
    let base = runtime.get_model("chatgpt-subscription", "codex").expect("base catalog model");
    let factory = NativeAsyncExtensionFactory {
        path: "<builtin:service-tier>".into(), source_info: Default::default(),
        factory: Arc::new(|api| {
            Box::pin(async move {
                ServiceTierExtension { host: Arc::new(FileTierHost) }.register(api);
                let inventory = api.events.on_native::<maho_ext_api::mcp_wire::McpControlInventoryRequest>(maho_ext_api::mcp_wire::MCP_CONTROL_INVENTORY_REQUEST_EVENT, Arc::new(|request| {
                    if let Some(sender) = request.respond.lock().expect("inventory sender").take() {
                        sender.send(json!({"servers":[{"name":"fixture-mcp","serverInfo":null,"tools":[],"resources":[],"resourceTemplates":[],"authStatus":"notLoggedIn"}]})).expect("inventory receiver");
                    }
                }));
                let inventory = Arc::new(std::sync::Mutex::new(Some(inventory)));
                api.on(maho_ext_api::EventKind::SessionShutdown, Arc::new(move |_,_| { inventory.lock().expect("inventory subscription").take(); Box::pin(async { Ok(maho_ext_api::EventResult::None) }) }));
                Ok(())
            })
        }),
    };
    let session = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(agent_dir.to_string_lossy().into_owned()),
        model_runtime: Some(runtime), model: Some(base),
        session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
        settings_manager: Some(SettingsManager::from_storage(Box::new(FileSettingsStorage::new(&cwd, &agent_dir.to_string_lossy(), "/unused-home")), false)),
        extension_factories: vec![factory], no_tools: Some(NoToolsMode::All), auto_title_sessions: Some(false), ..Default::default()
    }).await?.session;

    let surfaces = rpc(&session, json!({"type":"get_loaded_surfaces"})).await;
    assert_eq!(surfaces["success"], true);
    assert!(surfaces["data"]["extensions"].as_array().expect("extension inventory").iter().any(|extension|
        extension["path"] == "<builtin:service-tier>" && extension["enabled"] == true && extension["name"] == "service-tier"));
    assert_eq!(surfaces["data"]["mcpServers"], json!([{"name":"fixture-mcp","toolCount":0,"status":"needs_auth","authStatus":"notLoggedIn"}]));

    let enabled = rpc(&session, json!({"type":"set_fast_mode","enabled":true})).await;
    assert_eq!(enabled["success"], true);
    assert_eq!(enabled["data"]["enabled"], true);
    assert_eq!(enabled["data"]["modelId"], "codex");
    assert_eq!(session.model().id, "codex-fast");
    let state_path = agent_dir.join("state.json");
    let persisted: Value = serde_json::from_slice(&std::fs::read(&state_path)?)?;
    assert_eq!(persisted["modelServiceTiers"]["chatgpt-subscription/codex"], "priority");

    let disabled = rpc(&session, json!({"type":"set_fast_mode","enabled":false})).await;
    assert_eq!(disabled["success"], true);
    assert_eq!(disabled["data"]["enabled"], false);
    assert_eq!(disabled["data"]["modelId"], "codex");
    let persisted: Value = serde_json::from_slice(&std::fs::read(&state_path)?)?;
    assert_eq!(persisted["modelServiceTiers"]["chatgpt-subscription/codex"], "auto");

    let unsupported_model = session.model_runtime().get_model("faux", "faux-1").expect("unsupported faux model");
    session.set_model(unsupported_model).await?;
    let refused = rpc(&session, json!({"type":"set_fast_mode","enabled":true})).await;
    assert_eq!(refused["success"], false);
    assert!(refused["error"].as_str().is_some());
    assert!(!session.is_fast_mode_active());

    let base = session.model_runtime().get_model("chatgpt-subscription", "codex").expect("base model");
    session.set_model(base).await?;
    let enabled = rpc(&session, json!({"type":"set_fast_mode","enabled":true})).await;
    assert_eq!(enabled["success"], true);
    let context = session.extension_command_context().await.expect("bound extension command context").context;
    let preparation = context.session_manager.extension_context_actions().expect("bound session actions")
        .prepare_provider_request(Vec::new()).await?;
    let payload = (preparation.transform_payload)(json!({"model":"codex"})).await?;
    assert_eq!(payload["service_tier"], "priority");
    assert_eq!(payload["model"], "codex");

    let unsupported_model = session.model_runtime().get_model("faux", "faux-1").expect("unsupported faux model");
    session.set_session_model(unsupported_model).await?;
    let refused = rpc(&session, json!({"type":"set_fast_mode","enabled":true})).await;
    assert_eq!(refused["success"], false);
    assert!(refused["error"].as_str().is_some());
    assert!(!session.is_fast_mode_active());

    session.dispose().await;
    Ok(())
}
