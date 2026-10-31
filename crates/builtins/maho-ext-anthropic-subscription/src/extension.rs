use maho_ext_api::{Extension, ExtensionApi, ProviderConfig, types::ProviderModelConfig};
use std::sync::Arc;

pub struct AnthropicSubscriptionExtension {
    pub oauth: Arc<crate::oauth_login::AnthropicSubscriptionOAuth>,
    pub stream: maho_ext_api::types::ProviderStream,
    pub settings: Arc<dyn Fn() -> crate::settings::ProviderSettings + Send + Sync>,
    pub registry: Arc<tokio::sync::Mutex<crate::session_stream::SessionRegistry>>,
    pub watch: Arc<std::sync::Mutex<crate::tool_watch::ToolWatch>>,
}

impl AnthropicSubscriptionExtension {
    pub fn native(
        oauth: Arc<crate::oauth_login::AnthropicSubscriptionOAuth>,
        executable: std::path::PathBuf,
        source: Option<crate::executable::ExecutableSource>,
        cwd: std::path::PathBuf,
        agent_dir: std::path::PathBuf,
        environment: std::collections::BTreeMap<String, String>,
    ) -> Self {
        Self::native_with_resolver(oauth, Arc::new(move || -> anyhow::Result<(std::path::PathBuf, Option<crate::executable::ExecutableSource>)> { Ok((executable.clone(), source)) }), cwd, agent_dir, environment)
    }

    /// senpi `stream.ts`: the Claude Code executable is resolved inside every turn
    /// (`resolveClaudeCodeRun(defaultExecutableDeps())`), so a missing binary fails only the turn
    /// with senpi's resolution error and a later install needs no restart.
    pub fn native_with_resolver(
        oauth: Arc<crate::oauth_login::AnthropicSubscriptionOAuth>,
        resolve: crate::executable::ExecutableResolver,
        cwd: std::path::PathBuf,
        agent_dir: std::path::PathBuf,
        environment: std::collections::BTreeMap<String, String>,
    ) -> Self {
        let registry = Arc::new(tokio::sync::Mutex::new(
            crate::session_stream::SessionRegistry::default(),
        ));
        let watch = Arc::new(std::sync::Mutex::new(
            crate::tool_watch::ToolWatch::default(),
        ));
        let settings = oauth.settings.clone();
        let stream = {
            let oauth = oauth.clone();
            let registry = registry.clone();
            let watch = watch.clone();
            Arc::new(
                move |model: &maho_ai::model::Model, context: &maho_ai::types::Context, options| {
                    let (executable, source) = match resolve() {
                        Ok(resolved) => resolved,
                        Err(error) => {
                            return crate::stream::failed_stream(model, &error.to_string());
                        }
                    };
                    let settings = (oauth.settings)();
                    let settings = if settings.values.is_empty() {
                        // senpi#2848: settings come from the session cwd, never the host working directory.
                        match crate::settings::load_from_disk(
                            &cwd,
                            &agent_dir,
                            &dirs::home_dir().unwrap_or_else(|| cwd.clone()),
                            &environment,
                        ) {
                            Ok(settings) => settings,
                            Err(error) => {
                                return crate::stream::failed_stream(model, &error.to_string());
                            }
                        }
                    } else {
                        settings
                    };
                    crate::stream::stream_anthropic_subscription(
                        model.clone(),
                        context.clone(),
                        options,
                        crate::stream::StreamDeps {
                            executable,
                            source,
                            cwd: cwd.clone(),
                            agent_dir: agent_dir.clone(),
                            environment: environment.clone(),
                            settings,
                            store: oauth.store.clone(),
                            refresh: oauth.flow.clone(),
                            registry: registry.clone(),
                            watch: watch.clone(),
                            now: Arc::new(maho_ai::utils::diagnostics::now_ms),
                        },
                    )
                },
            ) as maho_ext_api::types::ProviderStream
        };
        Self {
            oauth,
            stream,
            settings,
            registry,
            watch,
        }
    }
}

/// The lane's models carry the window their requests actually get, which is what compaction plans against: the
/// catalog window, except a legacy long-context id at Claude Code's 200k default after the account refused its 1M
/// opt-in (senpi#3408, senpi#3801).
pub fn models() -> Vec<ProviderModelConfig> {
    models_for(crate::long_context::long_context_available())
}

pub fn models_for(long_context: bool) -> Vec<ProviderModelConfig> {
    maho_ai::models_generated::MODELS["anthropic"]
        .values()
        .map(|model| {
            let mut thinking = model.thinking_level_map.clone().unwrap_or_default();
            thinking.insert(maho_ai::types::ModelThinkingLevel::Minimal, None);
            ProviderModelConfig {
                model_type: None,
                output: None,
                image_cost: None,
                input_limits: model.input_limits.clone(),
                prompt_cache: model.prompt_cache.clone(),
                supports_assistant_prefill: model.supports_assistant_prefill,
                default_thinking_level: model.default_thinking_level,
                id: model.id.clone(),
                name: model.name.clone(),
                reasoning: model.reasoning,
                input: model.input.clone(),
                cost: model.cost.clone(),
                context_window: crate::context_window::planned_context_window(
                    &model.id,
                    model.context_window,
                    long_context,
                ),
                max_tokens: model.max_tokens,
                thinking_level_map: Some(thinking),
                upstream_model_id: None,
                api: None,
                base_url: None,
                recover_text_tool_calls: None,
                headers: None,
                extra_body: None,
                compat: None,
            }
        })
        .collect()
}

impl Extension for AnthropicSubscriptionExtension {
    fn register(&self, api: &mut ExtensionApi) {
        crate::session_registry_wiring::register(api, self.registry.clone());
        crate::tool_watch::register(api, self.watch.clone());
        crate::account_command::register(
            api,
            self.oauth.clone(),
            Arc::new(std::env::vars().collect()),
        );
        let settings = self.settings.clone();
        // Each registration is the whole config: the runtime validates a registration on its own, not merged with
        // the previous one.
        let config = ProviderConfig {
            owns_account_selection: true,
            base_url: Some(crate::api_id::CLAUDE_SDK_OAUTH_API_ID.into()),
            api: Some(crate::api_id::CLAUDE_SDK_OAUTH_API_ID.into()),
            models: None,
            stream_simple: Some(self.stream.clone()),
            oauth: Some(self.oauth.clone()),
            fallback_eligible: Some(Arc::new(move || {
                (settings)().values.get("enabled") != Some(&serde_json::Value::Bool(false))
            })),
            ..Default::default()
        };
        let provider_config = move |long_context: bool| ProviderConfig {
            models: Some(models_for(long_context)),
            ..config.clone()
        };
        api.register_provider(
            crate::auth_lane::PROVIDER_ID,
            provider_config(crate::long_context::long_context_available()),
        )
        .expect("Anthropic Subscription provider registration");
        // The lane re-registers whenever the 1M window is refused or offered again, so the session's current model,
        // and compaction with it, plans against the window the requests get.
        // deviation: the listener registry has no unsubscribe; session_shutdown turns this listener into a no-op.
        let following = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let runtime = api.runtime.clone();
        let path = api.registered.identity.path.clone();
        let listening = following.clone();
        crate::long_context::on_long_context_availability_change(move |available| {
            if !listening.load(std::sync::atomic::Ordering::SeqCst)
                || runtime.assert_active().is_err()
            {
                return;
            }
            if let Err(error) = runtime.register_provider(
                maho_ext_api::types::ProviderRegistration::Config {
                    name: crate::auth_lane::PROVIDER_ID.into(),
                    config: Box::new(provider_config(available)),
                },
                &path,
            ) {
                maho_ext_api::diagnostic(&format!(
                    "Anthropic Subscription long-context registration failed: {error}"
                ));
            }
        });
        api.on(
            maho_ext_api::EventKind::SessionShutdown,
            Arc::new(move |_, _| {
                following.store(false, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { Ok(maho_ext_api::EventResult::None) })
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_refused_long_context_plans_legacy_ids_at_the_standard_window() {
        let window = |long_context: bool, id: &str| {
            models_for(long_context)
                .into_iter()
                .find(|model| model.id == id)
                .map(|model| model.context_window)
        };
        assert_eq!(window(true, "claude-opus-4-6"), Some(1_000_000));
        assert_eq!(
            window(false, "claude-opus-4-6"),
            Some(crate::context_window::CLAUDE_CODE_DEFAULT_CONTEXT_WINDOW)
        );
    }
}
