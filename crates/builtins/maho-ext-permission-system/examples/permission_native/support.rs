#[path = "context.rs"]
mod context;

use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider, faux_streams};
use maho_core::agent_session::{AgentSession, AgentSessionConfig, ExtensionBindings, ExecuteToolOptions};
use maho_ext_api::*;
use maho_ext_host::{ExtensionRunner, loader::{NativeExtensionFactory, load_extensions}};
use maho_ext_permission_system::{PermissionSystem, storage};
use serde_json::{Value, json};
use std::{path::Path, sync::{Arc, Mutex}};

type Failure = Box<dyn std::error::Error + Send + Sync>;
struct ProofExtension;
impl Extension for ProofExtension {
    fn register(&self, api: &mut ExtensionApi) {
        PermissionSystem.register(api);
        api.set_flag("permission-preset", FlagValue::String("ask".into()));
        api.set_flag("permission", FlagValue::String("edit:blocked.txt=deny".into()));
        api.register_tool(maho_ext_gpt_apply_patch::tool::create_apply_patch_tool_variant(maho_ext_gpt_apply_patch::types::ApplyPatchWireMode::Json));
    }
}

async fn execute(project: &Path, allow: bool, tool: &str, input: Value, permission: &str) -> Result<Value, Failure> {
    execute_mode(project, allow, tool, input, permission, if allow { ExtensionMode::Tui } else { ExtensionMode::Print }).await
}
async fn execute_mode(project: &Path, allow: bool, tool: &str, input: Value, permission: &str, mode: ExtensionMode) -> Result<Value, Failure> {
    let provider = faux_provider(RegisterFauxProviderOptions { tokens_per_second: Some(0.0), ..Default::default() });
    let model = provider.get_model(Some("faux-1")).ok_or("Missing faux model")?;
    let streams = faux_streams(provider.core.clone());
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        models_path: Some(project.join("models.json")), auth_path: Some(project.join("auth.json")),
        providers: Some(vec![provider.provider.clone()]), ..Default::default()
    });
    let cwd = project.to_string_lossy().into_owned();
    let session = AgentSession::new(AgentSessionConfig {
        agent: maho_agent::Agent::new(maho_agent::AgentOptions {
            initial_state: Some(maho_agent::agent::PartialAgentState { model: Some(model), ..Default::default() }),
            stream_fn: Some(Arc::new(move |model, context, options| streams.stream_simple(model, context, options.map(|options| options.simple)))), ..Default::default()
        }),
        session_manager: maho_core::session_manager::SessionManager::in_memory(&cwd, None, None),
        settings_manager: maho_core::settings_manager::SettingsManager::from_storage(Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()), true),
        cwd: cwd.clone(), agent_dir: Some(cwd), fallback_now: Some(Arc::new(|| 0.0)), retry_random: Some(Arc::new(|| 0.5)),
        scoped_models: Vec::new(), favorite_models: Vec::new(), flag_values: [("permission-preset".into(), FlagValue::String("ask".into())), ("permission".into(), FlagValue::String(permission.into()))].into(), custom_tools: Vec::new(),
        model_runtime: Some(runtime), model_registry: None, uses_default_stream_function: Some(false),
        initial_active_tool_names: None, default_tool_names: None, eval_only_tool_names: None, allowed_tool_names: None,
        excluded_tool_names: None, base_tools_override: None, session_start_event: None, auto_title_sessions: Some(false),
    })?;
    let loaded = load_extensions(vec![NativeExtensionFactory { path: "<permission-native>".into(), source_info: SourceInfo::default(), extension: Box::new(ProofExtension) }], project, ExtensionSessionProfile::default());
    if !loaded.errors.is_empty() { return Err(format!("Factory errors: {:?}", loaded.errors).into()); }
    let telemetry = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = telemetry.clone();
    let _subscription = loaded.events.on("permission_asked", Arc::new(move |value| captured.lock().expect("telemetry").push(value.clone())));
    loaded.runtime.set_flag("permission-preset", FlagValue::String("ask".into()));
    loaded.runtime.set_flag("permission", FlagValue::String(permission.into()));
    let mut event_context = context::create(&session);
    event_context.has_ui = allow;
    event_context.mode = mode;
    event_context.ui = Arc::new(context::DecisionUi(allow));
    let runner = ExtensionRunner::new(loaded.extensions, loaded.runtime, loaded.events, event_context);
    session.set_extension_runner(runner).await;
    session.bind_extensions(ExtensionBindings { ui_context: allow.then(|| Arc::new(context::DecisionUi(true)) as Arc<dyn ExtensionUi>), mode: Some(mode), ..Default::default() }).await;
    let active_tools = session.get_active_tool_names();
    let result = session.execute_tool(tool, input, ExecuteToolOptions::default()).await;
    let execution = match result {
        Ok(result) => json!({"result":serde_json::to_value(result)?,"blocked":false}),
        Err(error) => json!({"blocked":true,"code":error.code,"message":error.message}),
    };
    session.emit_session_shutdown(SessionReason::Quit).await;
    session.dispose().await;
    let asked = telemetry.lock().expect("telemetry").clone();
    Ok(json!({"execution":execution,"asked":asked,"activeTools":active_tools}))
}

pub async fn mode_matrix() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let outcome = async {
        for mode in [ExtensionMode::Print, ExtensionMode::Json, ExtensionMode::Rpc, ExtensionMode::AppServer] {
            for (rule, admitted) in [("",false),("edit:mode.txt=allow",true),("edit:mode.txt=deny",false)] {
                let project = tempfile::tempdir_in(root.path())?;
                let result = execute_mode(project.path(),false,"apply_patch",json!({"input":"*** Begin Patch\n*** Add File: mode.txt\n+admitted\n*** End Patch"}),rule,mode).await?;
                assert_eq!(project.path().join("mode.txt").exists(),admitted,"{mode:?} {rule}");
                if admitted { assert_eq!(result["execution"]["blocked"],false); }
                else { assert_eq!(result["execution"]["code"],"blocked"); }
                assert_eq!(result["asked"].as_array().ok_or("asked")?.len(),usize::from(rule.is_empty()));
                project.close()?;
            }
        }
        Ok::<(),Failure>(())
    }.await;
    root.close()?;
    outcome
}

pub async fn run() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let project = root.path();
    let patch = json!({"input":"*** Begin Patch\n*** Add File: allowed.txt\n+native mutation\n*** End Patch"});
    let permission = "edit:blocked.txt=deny";
    let first = execute(project, true, "apply_patch", patch, permission).await?;
    assert_eq!(std::fs::read_to_string(project.join("allowed.txt"))?, "native mutation\n");
    let approved = storage::load_approved(project)?;
    println!("{}", json!({"stage":"first","result":first,"approved":approved}));
    assert!(approved.iter().any(|rule| rule.permission == "edit" && rule.pattern == "allowed.txt"));
    let second = execute(project, false, "apply_patch", json!({"input":"*** Begin Patch\n*** Update File: allowed.txt\n@@\n-native mutation\n+persisted mutation\n*** End Patch"}), permission).await?;
    assert_eq!(second["asked"], json!([]));
    assert_eq!(std::fs::read_to_string(project.join("allowed.txt"))?, "persisted mutation\n");
    let denied = execute(project, false, "apply_patch", json!({"input":"*** Begin Patch\n*** Add File: blocked.txt\n+forbidden\n*** End Patch"}), permission).await?;
    assert!(!project.join("blocked.txt").exists());
    assert_eq!(denied["execution"]["code"], "blocked");
    let mut edges = Vec::new();
    for input in [json!({"input":""}), json!({"input":"not a patch"})] {
        let edge = execute(project, false, "apply_patch", input, permission).await?;
        assert_eq!(edge["asked"][0]["permission"], "edit");
        assert_eq!(edge["asked"][0]["patterns"], json!(["*"]));
        assert_eq!(edge["execution"]["code"], "blocked");
        edges.push(edge);
    }
    let filtered = execute(project, false, "apply_patch", json!({"input":"*** Begin Patch\n*** Add File: filtered.txt\n+forbidden\n*** End Patch"}), "edit=deny").await?;
    assert!(!filtered["activeTools"].as_array().ok_or("active tools")?.iter().any(|name| name == "apply_patch"));
    assert!(!project.join("filtered.txt").exists());
    assert_eq!(filtered["execution"]["code"], "inactive_tool");
    assert_eq!(std::fs::read_to_string(project.join("allowed.txt"))?, "persisted mutation\n");
    println!("{}", json!({"first":first,"reload":second,"denied":denied,"edges":edges,"filtered":filtered,"approved":approved,"pass":true}));
    root.close()?;
    println!("cleanup: native sessions disposed; temporary project removed");
    Ok(())
}
