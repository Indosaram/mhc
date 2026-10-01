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

    fn test_model() -> Model {
        serde_json::from_value(serde_json::json!({
            "id": "faux-1", "name": "faux-1", "api": "faux", "provider": "faux",
            "baseUrl": "", "reasoning": true, "input": [], "contextWindow": 128000, "maxTokens": 4096,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }
        }))
        .expect("model")
    }
}
