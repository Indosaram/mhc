use std::sync::Arc;
use maho_ext_api::{Extension, ExtensionApi, ProviderConfig, types::ProviderModelConfig};

pub struct AnthropicSubscriptionExtension {
    pub oauth: Arc<crate::oauth_login::AnthropicSubscriptionOAuth>,
    pub stream: maho_ext_api::types::ProviderStream,
    pub settings: Arc<dyn Fn() -> crate::settings::ProviderSettings + Send + Sync>,
    pub registry: Arc<tokio::sync::Mutex<crate::session_stream::SessionRegistry>>,
}

pub fn models() -> Vec<ProviderModelConfig> {
    maho_ai::models_generated::MODELS["anthropic"].values().map(|model| {
        let mut thinking = model.thinking_level_map.clone().unwrap_or_default();
        thinking.insert(maho_ai::types::ModelThinkingLevel::Minimal, None);
        ProviderModelConfig { id: model.id.clone(), name: model.name.clone(), reasoning: model.reasoning, input: model.input.clone(), cost: model.cost.clone(), context_window: model.context_window, max_tokens: model.max_tokens, thinking_level_map: Some(thinking), upstream_model_id: None, api: None, base_url: None, recover_text_tool_calls: None, headers: None, extra_body: None, compat: None }
    }).collect()
}

impl Extension for AnthropicSubscriptionExtension {
    fn register(&self, api: &mut ExtensionApi) {
        crate::session_registry_wiring::register(api, self.registry.clone());
        crate::tool_watch::register(api, Arc::new(std::sync::Mutex::new(crate::tool_watch::ToolWatch::default())));
        crate::account_command::register(api, self.oauth.clone(), Arc::new(std::env::vars().collect()));
        let settings = self.settings.clone();
        api.register_provider(crate::auth_lane::PROVIDER_ID, ProviderConfig {
            base_url: Some(crate::api_id::CLAUDE_SDK_OAUTH_API_ID.into()), api: Some(crate::api_id::CLAUDE_SDK_OAUTH_API_ID.into()),
            models: Some(models()), stream_simple: Some(self.stream.clone()), oauth: Some(self.oauth.clone()),
            fallback_eligible: Some(Arc::new(move || (settings)().values.get("enabled") != Some(&serde_json::Value::Bool(false)))),
            ..Default::default()
        }).expect("Anthropic Subscription provider registration");
    }
}
