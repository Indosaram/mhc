use std::sync::{Arc, Mutex};
use maho_ai::{auth::types::*, oauth::*, types::Model, utils::abort::AbortSignal};
use maho_ext_api::ExtensionOAuthConfig;

pub struct ExtensionOAuthAdapter {
    config: Arc<dyn ExtensionOAuthConfig>,
}
impl ExtensionOAuthAdapter {
    pub fn new(config: Arc<dyn ExtensionOAuthConfig>) -> Self { Self { config } }
    pub fn install(self: &Arc<Self>, config: &mut maho_ext_api::ProviderConfig) { config.oauth = Some(self.clone()); }
    pub fn modify_models(&self, models: Vec<Model>, credential: &OAuthCredential) -> Vec<Model> {
        self.config.modify_models(models, credential)
    }
}

struct LoginCallbacks<'a> {
    interaction: &'a ProviderAuthInteraction,
    error: Mutex<Option<tokio::sync::oneshot::Sender<anyhow::Error>>>,
}
impl LoginCallbacks<'_> {
    async fn prompt(&self, prompt: AuthPrompt) -> String {
        match self.interaction.prompt(prompt).await {
            Ok(value) => value,
            Err(error) => {
                if let Some(sender) = self.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
                    && let Err(error) = sender.send(error) { eprintln!("Extension OAuth prompt failed: {error}"); }
                std::future::pending().await
            }
        }
    }
}
impl OAuthLoginCallbacks for LoginCallbacks<'_> {
    fn on_auth(&self, info: OAuthAuthInfo) { self.interaction.notify(AuthEvent::AuthUrl { url: info.url, instructions: info.instructions }); }
    fn on_device_code(&self, info: OAuthDeviceCodeInfo) {
        self.interaction.notify(AuthEvent::DeviceCode { user_code: info.user_code, verification_uri: info.verification_uri,
            interval_seconds: info.interval_seconds.map(|value| value as f64), expires_in_seconds: info.expires_in_seconds.map(|value| value as f64) });
    }
    fn on_prompt(&self, prompt: OAuthPrompt) -> maho_ai::types::BoxFuture<'_, String> {
        Box::pin(async move { self.prompt(AuthPrompt { kind: AuthPromptKind::Text { message: prompt.message, placeholder: prompt.placeholder }, signal: prompt.signal }).await })
    }
    fn on_progress(&self, message: &str) { self.interaction.notify(AuthEvent::Progress { message: message.into() }); }
    fn on_manual_code_input(&self) -> Option<maho_ai::types::BoxFuture<'_, String>> {
        Some(Box::pin(async move { self.prompt(AuthPrompt { kind: AuthPromptKind::ManualCode { message: "Paste the authorization code".into(), placeholder: None }, signal: Some(self.interaction.signal.clone()) }).await }))
    }
    fn on_select(&self, prompt: OAuthSelectPrompt) -> maho_ai::types::BoxFuture<'_, Option<String>> {
        Box::pin(async move {
            Some(self.prompt(AuthPrompt { kind: AuthPromptKind::Select { message: prompt.message, options: prompt.options.into_iter().map(|option| AuthPromptOption { id: option.id, label: option.label, description: option.description }).collect() }, signal: prompt.signal }).await)
        })
    }
    fn signal(&self) -> Option<AbortSignal> { Some(self.interaction.signal.clone()) }
}
#[async_trait::async_trait]
impl OAuthAuth for ExtensionOAuthAdapter {
    fn name(&self) -> &str { self.config.name() }
    fn is_subscription(&self) -> bool { self.config.is_subscription() }
    async fn login(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let callbacks = LoginCallbacks { interaction, error: Mutex::new(Some(sender)) };
        tokio::select! {
            result = self.config.login(&callbacks) => result.map_err(anyhow::Error::new),
            error = receiver => Err(error.map_err(anyhow::Error::new)?),
        }
    }
    async fn refresh(&self, credential: &OAuthCredential, signal: &AbortSignal) -> anyhow::Result<OAuthCredential> {
        self.config.refresh_token(credential, signal).await.map_err(anyhow::Error::new)
    }
    async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
        Ok(ModelAuth { api_key: Some(self.config.get_api_key(credential)), ..Default::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::{ExtensionFailure, ExtensionFuture};

    struct Config;
    impl ExtensionOAuthConfig for Config {
        fn name(&self) -> &str { "fixture" }
        fn is_subscription(&self) -> bool { true }
        fn uses_callback_server(&self) -> Option<bool> { Some(true) }
        fn login<'a>(&'a self, callbacks: &'a dyn OAuthLoginCallbacks) -> ExtensionFuture<'a, OAuthCredential> {
            Box::pin(async move {
                callbacks.on_progress("login");
                let access = callbacks.on_prompt(OAuthPrompt { message: "Token".into(), signal: callbacks.signal(), ..Default::default() }).await;
                Ok(OAuthCredential::new(access, "refresh", 1000.0))
            })
        }
        fn refresh_token<'a>(&'a self, credential: &'a OAuthCredential, signal: &'a AbortSignal) -> ExtensionFuture<'a, OAuthCredential> {
            Box::pin(async move {
                signal.throw_if_aborted().map_err(|error| ExtensionFailure::new(error.to_string()))?;
                Ok(OAuthCredential::new(format!("{}-new", credential.access), &credential.refresh, 2000.0))
            })
        }
        fn get_api_key(&self, credential: &OAuthCredential) -> String { credential.access.clone() }
    }
    struct Interaction { fail: bool }
    #[async_trait::async_trait]
    impl AuthInteraction for Interaction {
        fn signal(&self) -> Option<AbortSignal> { None }
        async fn prompt(&self, _: AuthPrompt) -> anyhow::Result<String> {
            if self.fail { anyhow::bail!("prompt cancelled"); }
            Ok("token".into())
        }
        fn notify(&self, _: AuthEvent) {}
    }
    #[tokio::test]
    async fn installed_oauth_runs_login_refresh_and_key_derivation() {
        let adapter = Arc::new(ExtensionOAuthAdapter::new(Arc::new(Config)));
        let mut config = maho_ext_api::ProviderConfig::default();
        adapter.install(&mut config);
        let oauth = config.oauth.unwrap();
        let signal = maho_ai::utils::abort::AbortController::new().signal();
        let interaction = ProviderAuthInteraction::new(signal.clone(), Arc::new(Interaction { fail: false }));
        let credential = oauth.login(&interaction).await.unwrap();
        assert!(oauth.is_subscription());
        let refreshed = oauth.refresh(&credential, &signal).await.unwrap();
        assert_eq!(oauth.to_auth(&refreshed).await.unwrap().api_key.as_deref(), Some("token-new"));
        assert_eq!(adapter.modify_models(vec![], &refreshed), vec![]);
    }
    #[tokio::test]
    async fn prompt_failure_returns_error_instead_of_panicking_or_continuing_login() {
        let adapter = ExtensionOAuthAdapter::new(Arc::new(Config));
        let interaction = ProviderAuthInteraction::new(maho_ai::utils::abort::AbortController::new().signal(), Arc::new(Interaction { fail: true }));
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), adapter.login(&interaction)).await.unwrap();
        assert_eq!(result.unwrap_err().to_string(), "prompt cancelled");
    }
}
