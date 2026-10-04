use std::sync::Arc;
use senpi_task::{host::HostError, runners::in_process::child_handle::{ChildSession, ChildSessionListener}};

struct NativeChild { session: Arc<maho_core::agent_session::AgentSession>, executor: tokio::runtime::Handle }
impl ChildSession for NativeChild {
    fn session_id(&self) -> String { self.session.session_id() }
    fn prompt(&self, text: &str) -> Result<(), HostError> {
        self.executor.block_on(async {
            self.session.prompt(text, Default::default()).await?;
            self.session.wait_for_idle().await;
            Ok::<(), String>(())
        }).map_err(|message| HostError { message })
    }
    fn steer(&self, text: &str) -> Result<(), HostError> { self.executor.block_on(self.session.steer(text, None, Default::default())).map_err(|message| HostError { message }) }
    fn follow_up(&self, text: &str) -> Result<(), HostError> { self.executor.block_on(self.session.follow_up(text, None, Default::default())).map_err(|message| HostError { message }) }
    fn abort(&self) { let session = self.session.clone(); self.executor.spawn(async move { session.abort().await; }); }
    fn subscribe(&self, listener: ChildSessionListener) -> senpi_task::manager::child_handle::Unsubscribe {
        let subscription = self.session.subscribe(Arc::new(move |event| { listener(&serde_json::to_value(event).expect("agent event serialization")); }));
        let session = self.session.clone();
        Box::new(move || { drop(subscription); drop(session); })
    }
    fn dispose(&self) { let session = self.session.clone(); self.executor.block_on(async move { session.emit_session_shutdown(maho_ext_api::SessionReason::Quit).await; session.dispose().await; }); }
}

pub fn factory(executor: tokio::runtime::Handle, parent: Arc<dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync>) -> senpi_task::runners::in_process::runner::CreateChildSession {
    Arc::new(move |options| {
        let model = options.model.as_ref().and_then(|model| model.downcast_ref::<maho_ai::model::Model>()).cloned();
        let registry = options.model_registry.as_ref().and_then(|registry| registry.downcast_ref::<super::task_runners::NativeChildModelRegistry>()).map(|registry| registry.0.clone());
        let models = options.model_runtime.as_ref().and_then(|runtime| runtime.downcast_ref::<maho_core::model_runtime::ModelRuntime>()).cloned();
        let manager = super::task_runners::native_child_session_manager(&options);
        let custom = options.custom_tools.iter().map(|tool| {
            let tool = tool.clone();
            let name = tool.name().to_owned(); let description = tool.description().to_owned();
            let parameters = parent().and_then(|parent| parent.get_tool_definition(&name)).map(|tool| tool.parameters)
                .ok_or_else(|| HostError { message: format!("Shared parent tool {name} has no native schema") })?;
            Ok(maho_ext_api::ToolDefinition::new(&name, &description, parameters,
                Arc::new(move |call| { let tool = tool.clone(); let id = call.id.to_owned(); let params = call.params; Box::pin(async move {
                    let result = tokio::task::spawn_blocking(move || tool.execute(&id, &params)).await
                        .map_err(|error| maho_tools::ToolError::Message(error.to_string()))?
                        .map_err(|error| maho_tools::ToolError::Message(error.message))?;
                    serde_json::from_value(result).map_err(|error| maho_tools::ToolError::Message(error.to_string()))
                }) })))
        }).collect::<Result<Vec<_>, HostError>>()?;
        let settings = super::task_runners::native_child_settings(&options.settings);
        let session = executor.block_on(maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
            cwd: Some(options.cwd), model, model_runtime: models, model_registry: registry,
            agent_dir: options.agent_dir,
            auth_storage: options.auth_storage.and_then(|auth| auth.downcast::<maho_core::auth_storage::AuthStorage>().ok()),
            session_manager: Some(manager), settings_manager: Some(settings), tools: options.tools, exclude_tools: options.exclude_tools,
            custom_tools: custom, minimal_resources: true,
            thinking_level: options.thinking_level.as_deref().and_then(maho_ai::types::ThinkingLevel::parse),
            ..Default::default()
        })).map_err(|message| HostError { message })?;
        Ok(Arc::new(NativeChild { session: Arc::new(session.session), executor: executor.clone() }) as Arc<dyn ChildSession>)
    })
}
