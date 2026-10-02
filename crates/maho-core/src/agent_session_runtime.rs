//! Port of senpi `packages/coding-agent/src/core/agent-session-runtime.ts`.
//!
//! Not ported (documented in `parity.d/21.md`): the session replacement flows (`switchSession`,
//! `newSession`, `fork`, `importFromJsonl` and `createAgentSessionRuntime`) all call
//! `AgentSession::abort` and `createReplacedSessionContext` plus a runtime factory from `sdk.ts`,
//! none of which are ported yet, so only the launch-profile types, the runtime holder's accessors
//! and `dispose` are here.

use crate::agent_session::AgentSession;
use crate::agent_session_services::{AgentSessionRuntimeDiagnostic, AgentSessionServices};
use crate::session_cwd::assert_session_cwd_exists;

pub struct ExtensionModelRuntimeActions(pub std::sync::Mutex<crate::model_runtime::ModelRuntime>);
impl maho_ext_api::ExtensionProviderActions for ExtensionModelRuntimeActions {
    fn register_provider(&self, registration: maho_ext_api::ProviderRegistration, _: &str) -> Result<(), maho_ext_api::ExtensionFailure> {
        let mut runtime = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (name, config, model_options, retry_policy) = match registration {
            maho_ext_api::ProviderRegistration::Native(provider) => { runtime.register_native_provider(provider); return Ok(()); }
            maho_ext_api::ProviderRegistration::Config { name, config } => (name, config, std::collections::BTreeMap::new(), None),
            maho_ext_api::ProviderRegistration::ConfigOptions { name, options } => (name, Box::new(options.config), options.model_options, options.retry_policy),
        };
        let apply_options = move |model: maho_ext_api::ProviderModelConfig| {
            let options = model_options.get(&model.id).cloned().unwrap_or_default();
            let mut model = extension_provider_model(model);
            model.service_tier = options.service_tier;
            model.prompt_preset = options.prompt_preset;
            model.sampling_params = options.sampling_params.map(|params| params.into_iter().collect());
            model.cache_retention = options.cache_retention;
            model
        };
        let models = config.models.map(|models| models.into_iter().map(&apply_options).collect());
        let refresh_models = config.refresh_models.map(|refresh| -> crate::provider_composer::ExtensionRefresh {
            std::sync::Arc::new(move |context| {
                let future = refresh(context);
                let apply_options = apply_options.clone();
                Box::pin(async move { future.await.map(|models| models.into_iter().map(apply_options).collect()).map_err(|error| error.message) })
            })
        });
        let input = crate::provider_composer::ProviderConfigInput {
            config: crate::model_config_schema::ModelsJsonProvider {
                name: config.name, base_url: config.base_url, api_key: config.api_key, api: config.api,
                headers: config.headers, extra_body: config.extra_body.and_then(|value| value.as_object().cloned()),
                auth_header: config.auth_header, models, ..Default::default()
            },
            stream_simple: config.stream_simple, oauth: config.oauth, fallback_eligible: config.fallback_eligible,
            refresh_models, retry_policy,
        };
        runtime.register_provider(&name, input).map_err(maho_ext_api::ExtensionFailure::new)
    }
    fn unregister_provider(&self, name: &str, _: &str) -> Result<(), maho_ext_api::ExtensionFailure> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).unregister_provider(name); Ok(())
    }
}
fn extension_provider_model(model: maho_ext_api::ProviderModelConfig) -> crate::model_config_schema::ModelsJsonModel {
    crate::model_config_schema::ModelsJsonModel {
        id: model.id, name: Some(model.name), upstream_model_id: model.upstream_model_id, api: model.api, base_url: model.base_url,
        reasoning: Some(model.reasoning), recover_text_tool_calls: model.recover_text_tool_calls, thinking_level_map: model.thinking_level_map,
        input: Some(model.input), cost: Some(model.cost), context_window: Some(model.context_window as f64), max_tokens: Some(model.max_tokens as f64),
        headers: model.headers, extra_body: model.extra_body.and_then(|value| value.as_object().cloned()), compat: model.compat.map(|compat| compat.0),
        ..Default::default()
    }
}

/// Thrown when `/import` references a JSONL file path that does not exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionImportFileNotFoundError {
    pub file_path: String,
}

impl std::fmt::Display for SessionImportFileNotFoundError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "File not found: {}", self.file_path)
    }
}

impl std::error::Error for SessionImportFileNotFoundError {}

/// Immutable flags selected when a runtime is first launched.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentSessionLaunchProfile {
    pub cwd: String,
    pub permission_preset: Option<String>,
    pub creation_model: Option<(String, String)>,
    pub initial_thinking_level: Option<String>,
    pub auto_title: Option<bool>,
}

/// Result returned by runtime creation.
pub struct CreateAgentSessionRuntimeResult {
    pub session: AgentSession,
    pub services: AgentSessionServices,
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
    pub model_fallback_message: Option<String>,
}

pub type RebindSession = Box<dyn Fn(&AgentSession) + Send + Sync>;
pub type SessionInvalidateHook = Box<dyn Fn() + Send + Sync>;

/// Owns the current AgentSession plus its cwd-bound services.
pub struct AgentSessionRuntime {
    rebind_session: Option<RebindSession>,
    before_session_invalidate: Option<SessionInvalidateHook>,
    session: AgentSession,
    services: AgentSessionServices,
    diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
    model_fallback_message: Option<String>,
    launch_profile: Option<AgentSessionLaunchProfile>,
}

impl AgentSessionRuntime {
    pub fn new(
        session: AgentSession,
        services: AgentSessionServices,
        diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
        model_fallback_message: Option<String>,
        launch_profile: Option<AgentSessionLaunchProfile>,
    ) -> Self {
        Self {
            rebind_session: None,
            before_session_invalidate: None,
            session,
            services,
            diagnostics,
            model_fallback_message,
            launch_profile,
        }
    }

    pub fn services(&self) -> &AgentSessionServices {
        &self.services
    }

    pub fn session(&self) -> &AgentSession {
        &self.session
    }

    pub fn cwd(&self) -> &str {
        &self.services.cwd
    }

    pub fn diagnostics(&self) -> &[AgentSessionRuntimeDiagnostic] {
        &self.diagnostics
    }

    pub fn model_fallback_message(&self) -> Option<&str> {
        self.model_fallback_message.as_deref()
    }

    pub fn launch_profile(&self) -> Option<&AgentSessionLaunchProfile> {
        self.launch_profile.as_ref()
    }

    pub fn set_rebind_session(&mut self, rebind: Option<RebindSession>) {
        self.rebind_session = rebind;
    }

    pub fn set_before_session_invalidate(&mut self, callback: Option<SessionInvalidateHook>) {
        self.before_session_invalidate = callback;
    }

    pub async fn dispose(&self) {
        self.session.emit_session_shutdown(maho_ext_api::SessionReason::Quit).await;
        if let Some(callback) = &self.before_session_invalidate {
            callback();
        }
        self.session.dispose().await;
    }
}

/// Reject a session manager whose cwd does not exist, exactly as the runtime factory does.
pub fn assert_runtime_cwd(
    manager: &crate::session_manager::SessionManager,
    cwd: &str,
) -> Result<(), crate::session_cwd::MissingSessionCwdError> {
    assert_session_cwd_exists(manager, cwd)
}

/// Extract the text of a user message's content, for `fork`'s selected text.
pub fn extract_user_message_text(content: &serde_json::Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    content
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter(|part| part.get("type").and_then(serde_json::Value::as_str) == Some("text"))
                .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_provider_registration_reaches_the_live_session_catalog() {
        use maho_ext_api::*;
        let runtime = crate::model_runtime::ModelRuntime::create_sync(crate::model_runtime::CreateModelRuntimeOptions { providers: Some(vec![]), ..Default::default() });
        let actions = std::sync::Arc::new(ExtensionModelRuntimeActions(std::sync::Mutex::new(runtime.clone())));
        let extension_runtime = ExtensionRuntime::default();
        let api = ExtensionApi::new(LoadedExtension::new("fixture", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), extension_runtime.clone());
        let model = ProviderModelConfig {
            id: "fixture-model".into(), name: "Fixture".into(), upstream_model_id: None, api: None, base_url: None,
            reasoning: false, recover_text_tool_calls: None, thinking_level_map: None, input: vec![maho_ai::types::InputModality::Text],
            cost: Default::default(), context_window: 4096, max_tokens: 512, headers: None, extra_body: None, compat: None,
        };
        api.register_provider_with_options("fixture", ProviderConfigOptions {
            config: ProviderConfig { base_url: Some("https://example.test/v1".into()), api: Some("openai-completions".into()), models: Some(vec![model]), ..Default::default() },
            model_options: std::collections::BTreeMap::from([("fixture-model".into(), ProviderModelOptions {
                service_tier: Some(maho_ai::types::ServiceTierPreference::Flex), prompt_preset: Some("fixture-prompt".into()),
                sampling_params: Some(serde_json::from_value(serde_json::json!({"temperature": 0.2})).unwrap()),
                cache_retention: Some(maho_ai::types::CacheRetention::Long),
            })]),
            retry_policy: Some(maho_ai::utils::retry_profile::profiles::SENPI_DEFAULT_RETRY_PROFILE.clone()),
        }).unwrap();
        assert!(runtime.get_model("fixture", "fixture-model").is_none());
        extension_runtime.bind_providers(actions).unwrap();
        let model = runtime.get_model("fixture", "fixture-model").unwrap();
        assert_eq!(model.context_window, 4096); assert_eq!(model.base_url, "https://example.test/v1");
        assert_eq!(model.cache_retention, Some(maho_ai::types::CacheRetention::Long));
        assert_eq!(model.sampling_params.as_ref().unwrap()["temperature"], serde_json::json!(0.2));
        let config = runtime.get_registered_provider_config("fixture").unwrap();
        assert_eq!(config.retry_policy.as_ref().unwrap().id, "senpi-default");
        let definition = &config.models.as_ref().unwrap()[0];
        assert_eq!(definition.service_tier, Some(maho_ai::types::ServiceTierPreference::Flex));
        assert_eq!(definition.prompt_preset.as_deref(), Some("fixture-prompt"));
        api.unregister_provider("fixture").unwrap();
        assert!(runtime.get_model("fixture", "fixture-model").is_none());
    }

    #[test]
    fn a_missing_import_file_names_the_path() {
        let error = SessionImportFileNotFoundError { file_path: "/tmp/missing.jsonl".to_owned() };
        assert_eq!(error.to_string(), "File not found: /tmp/missing.jsonl");
    }

    #[test]
    fn user_message_text_joins_text_parts_and_skips_others() {
        let content = serde_json::json!([
            { "type": "text", "text": "hello " },
            { "type": "image", "data": "x" },
            { "type": "text", "text": "world" }
        ]);
        assert_eq!(extract_user_message_text(&content), "hello world");
    }

    #[test]
    fn a_string_content_is_returned_verbatim() {
        assert_eq!(extract_user_message_text(&serde_json::json!("plain")), "plain");
    }
}
