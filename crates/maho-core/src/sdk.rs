//! Port of senpi `packages/coding-agent/src/core/sdk.ts`.
//!
//! Not ported (documented in `parity.d/21.md`): `createAgentSession` needs the resource loader,
//! the built-in tool factories (`tools/index.ts`, todo 18) and `AgentSession::_buildRuntime`, none
//! of which are ported yet, so only `clampThinkingLevelToModel` and the option/result shapes are
//! here. `setDefaultStreamFn(streamSimple)` at module load has no Rust counterpart because the
//! agent's default stream function is installed by the caller.

use maho_ai::model::Model;
use maho_ai::types::{ModelThinkingLevel, ThinkingLevel, ThinkingSelection};

use crate::agent_session::SessionModelEntry;
use crate::auth_storage::AuthStorage;
use crate::model_registry::ModelRegistry;
use crate::model_runtime::ModelRuntime;
use crate::session_manager::SessionManager;
use crate::settings_manager::SettingsManager;

#[derive(Default)]
pub struct CreateAgentSessionOptions {
    pub cwd: Option<String>,
    pub agent_dir: Option<String>,
    pub model_runtime: Option<ModelRuntime>,
    pub auth_storage: Option<std::sync::Arc<AuthStorage>>,
    pub model_registry: Option<ModelRegistry>,
    pub model: Option<Model>,
    pub initial_model_provenance: Option<String>,
    pub thinking_level: Option<ThinkingLevel>,
    pub thinking_selection: Option<ThinkingSelection>,
    pub scoped_models: Vec<SessionModelEntry>,
    pub favorite_models: Vec<SessionModelEntry>,
    pub no_tools: Option<NoToolsMode>,
    pub tools: Option<Vec<String>>,
    pub exclude_tools: Option<Vec<String>>,
    pub custom_tools: Vec<maho_ext_api::ToolDefinition>,
    pub context_files: Vec<maho_ext_api::ContextFile>,
    pub session_manager: Option<SessionManager>,
    pub settings_manager: Option<SettingsManager>,
    pub session_start_event: Option<maho_ext_api::SessionStartEvent>,
    pub auto_title_sessions: Option<bool>,
}

/// `noTools` suppression mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoToolsMode {
    All,
    Builtin,
}

/// Result from `createAgentSession`.
pub struct CreateAgentSessionResult {
    pub session: crate::agent_session::AgentSession,
    pub model_fallback_message: Option<String>,
}

pub async fn create_agent_session(mut options: CreateAgentSessionOptions) -> Result<CreateAgentSessionResult, String> {
    use std::{collections::BTreeMap, sync::Arc};
    let cwd = options.cwd.take().unwrap_or_else(|| std::env::current_dir().map(|path| path.to_string_lossy().into_owned()).unwrap_or_default());
    let agent_dir = options.agent_dir.take().unwrap_or_else(crate::config::get_agent_dir);
    let runtime = options.model_runtime.take().unwrap_or_else(|| ModelRuntime::create_sync(crate::model_runtime::CreateModelRuntimeOptions {
        credentials: options.auth_storage.take(), models_path: Some(std::path::Path::new(&agent_dir).join("models.json")),
        auth_path: Some(std::path::Path::new(&agent_dir).join("auth.json")), providers: None,
    }));
    let registry = options.model_registry.take().unwrap_or_else(|| ModelRegistry::new(runtime.clone()));
    let settings = options.settings_manager.take().unwrap_or_else(|| SettingsManager::create(&cwd, &agent_dir, &crate::config::home_dir(), false));
    let manager = options.session_manager.take().unwrap_or_else(|| SessionManager::create(&cwd, None, None));
    crate::session_cwd::assert_session_cwd_exists(&manager, &cwd).map_err(|error| error.to_string())?;
    let context = manager.build_context(manager.leaf_id());
    let model = options.model.take().or_else(|| context.model.as_ref().and_then(|(provider, id)| registry.find(provider, id)))
        .or_else(|| settings.get_string("defaultProvider").zip(settings.get_string("defaultModel")).and_then(|(provider,id)| registry.find(&provider,&id)))
        .or_else(|| registry.get_available().into_iter().next()).ok_or("No model available")?;
    let thinking_level = clamp_thinking_level_to_model(options.thinking_level, Some(&model));
    let mut definitions = maho_tools::index::create_all_tool_definitions(std::path::Path::new(&cwd), Default::default());
    for definition in &options.custom_tools { definitions.insert(definition.name.clone(), definition.clone()); }
    let mut base_tools = BTreeMap::new();
    for (name, definition) in definitions {
        let executor = definition.execute.clone();
        let parameters = serde_json::from_value(definition.parameters.clone()).map_err(|error| error.to_string())?;
        let tool = maho_agent::types::AgentTool {
            label: definition.label, prepare_arguments: None, replay: None, execution_mode: None,
            tool: maho_ai::types::Tool { name: name.clone(), description: definition.description, parameters, freeform: None, constrained_sampling: None },
            execute: Arc::new(move |id, params, agent_signal, _on_update| {
                let executor = executor.clone(); Box::pin(async move {
                    let signal = maho_tools::definition::AbortSignal::default();
                    let execution = executor(maho_tools::definition::ToolCall { id: &id, params, signal: signal.clone(), on_update: None, context: None });
                    tokio::pin!(execution);
                    let result = if let Some(agent_signal) = agent_signal {
                        tokio::select! { result = &mut execution => result, _ = agent_signal.cancelled() => { signal.abort(); execution.await } }
                    } else { execution.await };
                    match result {
                        Ok(result) => match serde_json::from_value(serde_json::to_value(result.content).unwrap_or(serde_json::Value::Null)) {
                            Ok(content) => maho_agent::types::AgentToolResult { content, details: result.details.unwrap_or(serde_json::Value::Null),
                                usage: None, added_tool_names: None, terminate: None, is_error: None },
                            Err(error) => { let mut result = maho_agent::types::AgentToolResult::text(error.to_string()); result.is_error = Some(true); result }
                        },
                        Err(error) => { let mut result = maho_agent::types::AgentToolResult::text(error.to_string()); result.is_error = Some(true); result }
                    }
                })
            }),
        };
        base_tools.insert(name, tool);
    }
    let selected = if options.no_tools.is_some() { Vec::new() } else { options.tools.clone().unwrap_or_else(|| vec!["read".to_owned(),"bash".to_owned(),"edit".to_owned(),"write".to_owned()]) };
    let active_tools = selected.iter().filter_map(|name| base_tools.get(name).cloned()).collect();
    let runtime_for_stream = runtime.clone();
    let runtime_for_auth = runtime.clone();
    let messages = context.messages.into_iter().map(crate::agent_session::session_message_from_value)
        .collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())?;
    let agent = maho_agent::agent::Agent::new(maho_agent::agent::AgentOptions {
        initial_state: Some(maho_agent::agent::PartialAgentState { model: Some(model), thinking_level: Some(thinking_level),
            thinking_selection: options.thinking_selection, messages: Some(messages), tools: Some(active_tools), ..Default::default() }),
        stream_fn: Some(Arc::new(move |model, context, options| runtime_for_stream.stream_simple(model, context, options.map(|options| options.simple)))),
        get_api_key: Some(Arc::new(move |provider| { let runtime = runtime_for_auth.clone(); Box::pin(async move {
            runtime.get_auth(&provider).await.ok().flatten().and_then(|auth| auth.auth.api_key)
        }) })),
        ..Default::default()
    });
    let session = crate::agent_session::AgentSession::new(crate::agent_session::AgentSessionConfig {
        agent, session_manager: manager, settings_manager: settings, cwd: cwd.clone(), agent_dir: Some(agent_dir.clone()),
        fallback_now: None, retry_random: None, scoped_models: options.scoped_models, favorite_models: options.favorite_models,
        flag_values: BTreeMap::new(), custom_tools: options.custom_tools, model_runtime: Some(runtime), model_registry: Some(registry),
        uses_default_stream_function: Some(true), initial_active_tool_names: Some(selected),
        default_tool_names: None, eval_only_tool_names: None, allowed_tool_names: None, excluded_tool_names: options.exclude_tools,
        base_tools_override: Some(base_tools), session_start_event: None, auto_title_sessions: Some(false),
    }).map_err(|error| error.to_string())?;
    let templates = crate::prompt_templates::load_prompt_templates(&crate::prompt_templates::LoadPromptTemplatesOptions {
        cwd: cwd.clone(), agent_dir: agent_dir.clone(), include_defaults: true, ..Default::default()
    });
    let skills = crate::skills::load_skills(&crate::skills::LoadSkillsOptions { cwd, agent_dir, include_defaults: true, ..Default::default() });
    session.set_prompt_resources(templates, skills.skills);
    session.rebuild_system_prompt();
    Ok(CreateAgentSessionResult { session, model_fallback_message: None })
}

/// The default global config directory.
pub fn get_default_agent_dir() -> String {
    crate::config::get_agent_dir()
}

/// Clamp a requested thinking level to what the model supports, preferring lower levels.
pub fn clamp_thinking_level_to_model(level: Option<ThinkingLevel>, model: Option<&Model>) -> ModelThinkingLevel {
    let Some(model) = model else {
        return ModelThinkingLevel::Off;
    };
    if !model.reasoning {
        return ModelThinkingLevel::Off;
    }
    let requested = level.map_or(ModelThinkingLevel::Off, thinking_to_model_level);
    let available = crate::thinking_levels::get_supported_thinking_levels(model);
    if available.contains(&requested) {
        return requested;
    }
    let ordered = ModelThinkingLevel::ALL;
    let requested_index = ordered.iter().position(|candidate| *candidate == requested).unwrap_or(0);
    for candidate in ordered[..requested_index].iter().rev() {
        if available.contains(candidate) {
            return *candidate;
        }
    }
    for candidate in &ordered[requested_index + 1..] {
        if available.contains(candidate) {
            return *candidate;
        }
    }
    available.first().copied().unwrap_or(ModelThinkingLevel::Off)
}

fn thinking_to_model_level(level: ThinkingLevel) -> ModelThinkingLevel {
    match level {
        ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
        ThinkingLevel::Low => ModelThinkingLevel::Low,
        ThinkingLevel::Medium => ModelThinkingLevel::Medium,
        ThinkingLevel::High => ModelThinkingLevel::High,
        ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
        ThinkingLevel::Max => ModelThinkingLevel::Max,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_reasoning_model_clamps_to_off() {
        let mut model = test_model();
        model.reasoning = false;
        assert_eq!(clamp_thinking_level_to_model(Some(ThinkingLevel::High), Some(&model)), ModelThinkingLevel::Off);
    }

    #[test]
    fn an_unsupported_level_falls_back_to_a_lower_supported_one() {
        let model = test_model();
        assert_eq!(
            clamp_thinking_level_to_model(Some(ThinkingLevel::Xhigh), Some(&model)),
            ModelThinkingLevel::High
        );
    }

    #[test]
    fn a_missing_model_clamps_to_off() {
        assert_eq!(clamp_thinking_level_to_model(Some(ThinkingLevel::High), None), ModelThinkingLevel::Off);
    }

    #[tokio::test]
    async fn sdk_creates_session_with_working_builtin_tool() {
        let dir = tempfile::tempdir().expect("directory");
        let created = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(dir.path().to_string_lossy().into_owned()), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
            model: Some(test_model()), session_manager: Some(SessionManager::in_memory(&dir.path().to_string_lossy(), None, None)),
            tools: Some(vec!["bash".to_owned()]), ..Default::default()
        }).await.expect("SDK session");
        assert_eq!(created.session.get_active_tool_names(), vec!["bash"]);
        let result = created.session.execute_tool("bash", serde_json::json!({"command":"printf sdk"}), Default::default()).await.expect("tool");
        assert!(maho_ai::utils::text::content_text(&result.content, "").contains("sdk"));
    }

    fn test_model() -> Model {
        serde_json::from_value(serde_json::json!({
            "id": "faux-1", "name": "faux-1", "api": "faux", "provider": "faux",
            "baseUrl": "", "reasoning": true, "input": [], "contextWindow": 128000, "maxTokens": 4096,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }
        }))
        .expect("model")
    }
}
