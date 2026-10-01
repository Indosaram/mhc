//! Port of senpi packages/ai/src/auth/oauth/load.ts.

use crate::auth::types::OAuthAuth;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

pub type FlowFuture = Pin<Box<dyn Future<Output = anyhow::Result<Arc<dyn OAuthAuth>>> + Send>>;
pub type FlowLoader = Arc<dyn Fn() -> FlowFuture + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RadiusOAuthOptions {
    pub name: String,
    pub gateway: String,
}

pub type RadiusFlowLoader = Arc<dyn Fn(RadiusOAuthOptions) -> FlowFuture + Send + Sync>;

#[derive(Clone)]
pub struct OAuthFlowLoaders {
    pub anthropic: FlowLoader,
    pub chatgpt_subscription: FlowLoader,
    pub github_copilot: FlowLoader,
    pub openrouter: FlowLoader,
    pub kimi_coding: FlowLoader,
    pub xai: FlowLoader,
    pub cursor: FlowLoader,
    pub devin: FlowLoader,
    pub radius: RadiusFlowLoader,
}

static BUNDLED_LOADERS: Mutex<Option<OAuthFlowLoaders>> = Mutex::new(None);

pub fn register_bundled_oauth_flow_loaders(loaders: OAuthFlowLoaders) {
    *BUNDLED_LOADERS.lock().unwrap_or_else(|p| p.into_inner()) = Some(loaders);
}

fn bundled_loader<F>(pick: F) -> Option<FlowLoader>
where
    F: FnOnce(&OAuthFlowLoaders) -> FlowLoader,
{
    BUNDLED_LOADERS.lock().unwrap_or_else(|p| p.into_inner()).as_ref().map(pick)
}

fn bundled_radius_loader() -> Option<RadiusFlowLoader> {
    BUNDLED_LOADERS.lock().unwrap_or_else(|p| p.into_inner()).as_ref().map(|loaders| loaders.radius.clone())
}

pub async fn load_anthropic_oauth() -> anyhow::Result<Arc<dyn OAuthAuth>> {
    if let Some(loader) = bundled_loader(|loaders| loaders.anthropic.clone()) {
        return loader().await;
    }
    Ok(crate::auth::oauth::anthropic::anthropic_oauth())
}

pub async fn load_chatgpt_subscription_oauth() -> anyhow::Result<Arc<dyn OAuthAuth>> {
    if let Some(loader) = bundled_loader(|loaders| loaders.chatgpt_subscription.clone()) {
        return loader().await;
    }
    Ok(crate::auth::oauth::chatgpt_subscription::chatgpt_subscription_oauth())
}

pub async fn load_github_copilot_oauth() -> anyhow::Result<Arc<dyn OAuthAuth>> {
    if let Some(loader) = bundled_loader(|loaders| loaders.github_copilot.clone()) {
        return loader().await;
    }
    Ok(crate::auth::oauth::github_copilot::github_copilot_oauth())
}

pub async fn load_open_router_oauth() -> anyhow::Result<Arc<dyn OAuthAuth>> {
    if let Some(loader) = bundled_loader(|loaders| loaders.openrouter.clone()) {
        return loader().await;
    }
    Ok(crate::auth::oauth::openrouter::open_router_oauth())
}

pub async fn load_kimi_coding_oauth() -> anyhow::Result<Arc<dyn OAuthAuth>> {
    if let Some(loader) = bundled_loader(|loaders| loaders.kimi_coding.clone()) {
        return loader().await;
    }
    Ok(crate::auth::oauth::kimi_coding::kimi_coding_oauth())
}

pub async fn load_xai_oauth() -> anyhow::Result<Arc<dyn OAuthAuth>> {
    if let Some(loader) = bundled_loader(|loaders| loaders.xai.clone()) {
        return loader().await;
    }
    Ok(crate::auth::oauth::xai::xai_oauth())
}

pub async fn load_cursor_oauth() -> anyhow::Result<Arc<dyn OAuthAuth>> {
    if let Some(loader) = bundled_loader(|loaders| loaders.cursor.clone()) {
        return loader().await;
    }
    Ok(Arc::new(crate::auth::oauth::cursor::CursorOAuth::new()))
}

pub async fn load_devin_oauth() -> anyhow::Result<Arc<dyn OAuthAuth>> {
    if let Some(loader) = bundled_loader(|loaders| loaders.devin.clone()) {
        return loader().await;
    }
    Ok(crate::auth::oauth::devin::devin_oauth())
}

pub async fn load_radius_oauth(options: RadiusOAuthOptions) -> anyhow::Result<Arc<dyn OAuthAuth>> {
    if let Some(loader) = bundled_radius_loader() {
        return loader(options).await;
    }
    Ok(Arc::new(crate::auth::oauth::radius::create_radius_oauth(
        &options.name,
        &options.gateway,
        crate::auth::oauth::transport::default_transport(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::types::{ModelAuth, OAuthCredential, ProviderAuthInteraction};
    use crate::utils::abort::AbortSignal;
    use async_trait::async_trait;

    struct NamedOAuth(&'static str);

    #[async_trait]
    impl OAuthAuth for NamedOAuth {
        fn name(&self) -> &str {
            self.0
        }
        async fn login(&self, _interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
            Ok(OAuthCredential::new("a", "r", 1.0))
        }
        async fn refresh(&self, credential: &OAuthCredential, _signal: &AbortSignal) -> anyhow::Result<OAuthCredential> {
            Ok(credential.clone())
        }
        async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
            Ok(ModelAuth { api_key: Some(credential.access.clone()), ..Default::default() })
        }
    }

    fn loader(name: &'static str) -> FlowLoader {
        Arc::new(move || Box::pin(async move { Ok(Arc::new(NamedOAuth(name)) as Arc<dyn OAuthAuth>) }))
    }

    fn all_loaders(prefix: &'static str) -> OAuthFlowLoaders {
        OAuthFlowLoaders {
            anthropic: loader(prefix),
            chatgpt_subscription: loader(prefix),
            github_copilot: loader(prefix),
            openrouter: loader(prefix),
            kimi_coding: loader(prefix),
            xai: loader(prefix),
            cursor: loader(prefix),
            devin: loader(prefix),
            radius: Arc::new(move |options| {
                Box::pin(async move { Ok(Arc::new(NamedOAuth(options.name.leak())) as Arc<dyn OAuthAuth>) })
            }),
        }
    }

    #[tokio::test]
    async fn bundled_loaders_win_over_the_built_in_flows() {
        register_bundled_oauth_flow_loaders(all_loaders("bundled"));
        assert_eq!(load_anthropic_oauth().await.unwrap().name(), "bundled");
        assert_eq!(load_chatgpt_subscription_oauth().await.unwrap().name(), "bundled");
        assert_eq!(load_github_copilot_oauth().await.unwrap().name(), "bundled");
        assert_eq!(load_open_router_oauth().await.unwrap().name(), "bundled");
        assert_eq!(load_kimi_coding_oauth().await.unwrap().name(), "bundled");
        assert_eq!(load_xai_oauth().await.unwrap().name(), "bundled");
        assert_eq!(load_cursor_oauth().await.unwrap().name(), "bundled");
        assert_eq!(load_devin_oauth().await.unwrap().name(), "bundled");
        assert_eq!(
            load_radius_oauth(RadiusOAuthOptions { name: "radius-bundled".into(), gateway: "gw".into() })
                .await
                .unwrap()
                .name(),
            "radius-bundled"
        );
    }

    #[tokio::test]
    async fn radius_loader_receives_the_options() {
        register_bundled_oauth_flow_loaders(all_loaders("bundled"));
        let loaded = load_radius_oauth(RadiusOAuthOptions { name: "radius-x".into(), gateway: "gw".into() }).await.unwrap();
        assert_eq!(loaded.name(), "radius-x");
    }
}

/// Port of the `oauth-auth.test.ts` cases: the extension OAuth barrel, the subscription flags and
/// each flow's `toAuth`/`refresh` derivation.
#[cfg(test)]
mod oauth_auth_tests {
    use crate::auth::oauth::anthropic::{AnthropicOAuth, anthropic_oauth};
    use crate::auth::oauth::chatgpt_subscription::chatgpt_subscription_oauth;
    use crate::auth::oauth::cursor::CursorOAuth;
    use crate::auth::oauth::github_copilot::{GitHubCopilotOAuth, github_copilot_oauth};
    use crate::auth::oauth::kimi_coding::kimi_coding_oauth;
    use crate::auth::oauth::openrouter::open_router_oauth;
    use crate::auth::oauth::transport::{ScriptedResponse, ScriptedTransport};
    use crate::auth::oauth::xai::xai_oauth;
    use crate::auth::types::{ModelAuth, OAuthAuth, OAuthCredential};
    use crate::utils::abort::{AbortController, AbortSignal};
    use std::sync::Arc;

    fn signal() -> AbortSignal {
        AbortController::new().signal()
    }

    fn api_key_auth(api_key: &str) -> ModelAuth {
        ModelAuth { api_key: Some(api_key.to_string()), ..Default::default() }
    }

    #[tokio::test]
    async fn keeps_the_extension_oauth_barrel_free_of_built_in_flow_implementations() {
        // senpi asserts a runtime property on the module namespace: `src/oauth.ts` must not
        // re-export the built-in flow implementations. In Rust the barrel's surface is static, so
        // the observable halves pinned here are that the barrel carries the extension
        // compatibility types and yields flows only through the lazy loader seam - the only way
        // to obtain a flow from it is a loader call, never an eager implementation re-export.
        let register: fn(crate::auth::oauth::load::OAuthFlowLoaders) = crate::oauth::register_bundled_oauth_flow_loaders;
        let _ = register;
        let credentials: crate::oauth::OAuthCredentials = OAuthCredential::default();
        assert_eq!(credentials.access, "");
        let loaded = crate::oauth::load_anthropic_oauth().await.unwrap();
        assert!(!loaded.name().is_empty(), "the barrel yields flows through the loader");
    }

    #[test]
    fn identifies_only_subscription_backed_oauth_flows_as_subscriptions() {
        let flows: Vec<Arc<dyn OAuthAuth>> = vec![
            anthropic_oauth(),
            chatgpt_subscription_oauth(),
            github_copilot_oauth(),
            kimi_coding_oauth(),
            xai_oauth(),
            Arc::new(CursorOAuth::new()),
        ];
        for oauth in flows {
            assert!(oauth.is_subscription(), "{} must be subscription-backed", oauth.name());
        }
        assert!(!open_router_oauth().is_subscription());
    }

    #[tokio::test]
    async fn anthropic_to_auth_derives_the_api_key_from_the_access_token() {
        let auth = anthropic_oauth().to_auth(&OAuthCredential::new("token", "r", 0.0)).await.unwrap();
        assert_eq!(auth, api_key_auth("token"));
    }

    #[tokio::test]
    async fn openai_codex_to_auth_derives_the_api_key_from_the_access_token() {
        let auth = chatgpt_subscription_oauth().to_auth(&OAuthCredential::new("token", "r", 0.0)).await.unwrap();
        assert_eq!(auth, api_key_auth("token"));
    }

    #[tokio::test]
    async fn openrouter_derives_the_api_key_and_keeps_the_permanent_credential_on_refresh() {
        let credential = OAuthCredential::new("token", "", f64::MAX);
        assert_eq!(open_router_oauth().to_auth(&credential).await.unwrap(), api_key_auth("token"));
        assert_eq!(open_router_oauth().refresh(&credential, &signal()).await.unwrap(), credential);
    }

    #[tokio::test]
    async fn xai_to_auth_derives_the_api_key_from_the_access_token() {
        let auth = xai_oauth().to_auth(&OAuthCredential::new("token", "r", 0.0)).await.unwrap();
        assert_eq!(auth, api_key_auth("token"));
    }

    #[tokio::test]
    async fn github_copilot_to_auth_derives_base_url_from_the_token_proxy_endpoint() {
        let access = "tid=abc;exp=123;proxy-ep=proxy.enterprise.example;rest";
        let auth = github_copilot_oauth().to_auth(&OAuthCredential::new(access, "r", 0.0)).await.unwrap();
        assert_eq!(
            auth,
            ModelAuth { api_key: Some(access.into()), base_url: Some("https://api.enterprise.example".into()), ..Default::default() }
        );
    }

    #[tokio::test]
    async fn github_copilot_to_auth_falls_back_to_the_enterprise_domain_then_the_individual_endpoint() {
        let oauth = github_copilot_oauth();
        let enterprise = oauth
            .to_auth(
                &OAuthCredential::new("no-proxy-ep", "r", 0.0)
                    .with_extra("enterpriseUrl", serde_json::json!("https://company.ghe.com")),
            )
            .await
            .unwrap();
        assert_eq!(enterprise.base_url.as_deref(), Some("https://copilot-api.company.ghe.com"));

        let individual = oauth.to_auth(&OAuthCredential::new("no-proxy-ep", "r", 0.0)).await.unwrap();
        assert_eq!(individual.base_url.as_deref(), Some("https://api.individual.githubcopilot.com"));
    }

    #[tokio::test]
    async fn anthropic_refresh_exchanges_the_refresh_token_and_returns_a_typed_credential() {
        let transport = Arc::new(ScriptedTransport::new(vec![(
            Some("/oauth/token"),
            ScriptedResponse::Json {
                status: 200,
                body: serde_json::json!({"access_token": "new-access", "refresh_token": "new-refresh", "expires_in": 3600}),
            },
        )]));
        transport.set_now_ms(1_700_000_000_000.0);
        let oauth = AnthropicOAuth::new(transport);

        let refreshed = oauth.refresh(&OAuthCredential::new("old", "old-r", 0.0), &signal()).await.unwrap();

        assert_eq!(refreshed.access, "new-access");
        assert_eq!(refreshed.refresh, "new-refresh");
        assert!(refreshed.expires > 1_700_000_000_000.0, "{}", refreshed.expires);
    }

    #[tokio::test]
    async fn github_copilot_refresh_preserves_the_enterprise_domain() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            (
                Some("/copilot_internal/v2/token"),
                ScriptedResponse::Json { status: 200, body: serde_json::json!({"token": "new-token", "expires_at": 9_999_999_999i64}) },
            ),
            (Some("/models"), ScriptedResponse::Json { status: 200, body: serde_json::json!({"data": []}) }),
        ]));
        let oauth = GitHubCopilotOAuth::new(transport.clone());
        let credential = OAuthCredential::new("old", "gh-token", 0.0).with_extra("enterpriseUrl", serde_json::json!("company.ghe.com"));

        let refreshed = oauth.refresh(&credential, &signal()).await.unwrap();

        assert_eq!(refreshed.access, "new-token");
        assert_eq!(refreshed.get_extra_str("enterpriseUrl"), Some("company.ghe.com"));
        assert!(transport.requests()[0].url.contains("api.company.ghe.com"), "{:?}", transport.requests()[0].url);
    }
}
