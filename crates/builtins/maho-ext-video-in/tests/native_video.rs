#[path = "native_video/context.rs"]
mod context;
use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider, faux_streams};
use maho_core::agent_session::{AgentSession, AgentSessionConfig, ExtensionBindings, ExecuteToolOptions};
use maho_ext_api::*;
use maho_ext_host::{ExtensionRunner, loader::{load_extensions, NativeExtensionFactory}};
use serde_json::json;
use std::sync::Arc;
#[tokio::test]
async fn registered_video_activation_and_attachment() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let root = tempfile::tempdir()?;
    let project = root.path();
    let provider = faux_provider(RegisterFauxProviderOptions { tokens_per_second: Some(0.0), ..Default::default() });
    let mut model = provider.get_model(Some("faux-1")).ok_or("Missing faux model")?;
    model.input.push(maho_ai::types::InputModality::Video);
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
        scoped_models: Vec::new(), favorite_models: Vec::new(), flag_values: Default::default(), custom_tools: Vec::new(),
        model_runtime: Some(runtime), model_registry: None, uses_default_stream_function: Some(false),
        initial_active_tool_names: None, default_tool_names: None, eval_only_tool_names: None, allowed_tool_names: None,
        excluded_tool_names: None, base_tools_override: None, session_start_event: None, auto_title_sessions: Some(false),
    })?;

    let loaded = load_extensions(vec![NativeExtensionFactory {path:"<video-native>".into(),source_info:SourceInfo::default(),extension:Box::new(maho_ext_video_in::VideoIn)}],project,ExtensionSessionProfile::default());
    let (opened, _) = tokio::sync::mpsc::unbounded_channel();
    let (_, responses) = tokio::sync::watch::channel(None);
    let ui = Arc::new(context::DecisionUi {opened,responses});
    let runner = ExtensionRunner::new(loaded.extensions,loaded.runtime,loaded.events,context::create(&session,ui.clone()));
    session.set_extension_runner(runner).await;
    session.bind_extensions(ExtensionBindings {ui_context:Some(ui),mode:Some(ExtensionMode::Tui),..Default::default()}).await;
    let outcome = async {
        assert!(session.get_active_tool_names().iter().any(|name|name=="read_video"));
        std::fs::write(project.join("clip.mp4"),b"fake-mp4-bytes")?;
        let result = session.execute_tool("read_video",json!({"path":"clip.mp4"}),ExecuteToolOptions::default()).await?;
        assert_ne!(result.is_error,Some(true));
        assert!(result.content.iter().any(|content|matches!(content,maho_ai::types::ContentBlock::Image(image) if image.mime_type=="video/mp4" && image.data=="ZmFrZS1tcDQtYnl0ZXM=")));
        let mut text_model=session.model();
        text_model.input.retain(|input|*input!=maho_ai::types::InputModality::Video);
        session.set_session_model(text_model).await?;
        assert!(!session.get_active_tool_names().iter().any(|name|name=="read_video"));
        Ok::<(),Box<dyn std::error::Error + Send + Sync>>(())
    }.await;
    session.emit_session_shutdown(SessionReason::Quit).await;
    session.dispose().await;
    root.close()?;
    outcome
}
