//! Port of senpi packages/ai/src/bun-oauth.ts.

use crate::auth::oauth::load::{FlowLoader, OAuthFlowLoaders, RadiusOAuthOptions, register_bundled_oauth_flow_loaders};
use crate::auth::types::OAuthAuth;
use std::sync::Arc;

fn flow(load: fn() -> Arc<dyn OAuthAuth>) -> FlowLoader {
    Arc::new(move || Box::pin(async move { Ok(load()) }))
}

pub fn register_bun_oauth_flows() {
    register_bundled_oauth_flow_loaders(OAuthFlowLoaders {
        anthropic: flow(crate::auth::oauth::anthropic::anthropic_oauth),
        chatgpt_subscription: flow(crate::auth::oauth::chatgpt_subscription::chatgpt_subscription_oauth),
        github_copilot: flow(crate::auth::oauth::github_copilot::github_copilot_oauth),
        openrouter: flow(crate::auth::oauth::openrouter::open_router_oauth),
        kimi_coding: flow(crate::auth::oauth::kimi_coding::kimi_coding_oauth),
        xai: flow(crate::auth::oauth::xai::xai_oauth),
        cursor: Arc::new(|| Box::pin(async { Ok(Arc::new(crate::auth::oauth::cursor::CursorOAuth::new()) as Arc<dyn OAuthAuth>) })),
        devin: flow(crate::auth::oauth::devin::devin_oauth),
        radius: Arc::new(|options: RadiusOAuthOptions| {
            Box::pin(async move {
                Ok(Arc::new(crate::auth::oauth::radius::create_radius_oauth(
                    &options.name,
                    &options.gateway,
                    crate::auth::oauth::transport::default_transport(),
                )) as Arc<dyn OAuthAuth>)
            })
        }),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::oauth::load::{
        load_anthropic_oauth, load_chatgpt_subscription_oauth, load_cursor_oauth, load_devin_oauth,
        load_github_copilot_oauth, load_kimi_coding_oauth, load_open_router_oauth, load_xai_oauth,
    };

    #[tokio::test]
    async fn registers_every_bundled_flow_for_the_standalone_binary() {
        register_bun_oauth_flows();
        assert_eq!(load_anthropic_oauth().await.unwrap().name(), "Anthropic (Claude Pro/Max)");
        assert_eq!(load_chatgpt_subscription_oauth().await.unwrap().name(), "ChatGPT Subscription (Plus/Pro)");
        assert_eq!(load_github_copilot_oauth().await.unwrap().name(), "GitHub Copilot");
        assert_eq!(load_open_router_oauth().await.unwrap().name(), "OpenRouter OAuth");
        assert_eq!(load_kimi_coding_oauth().await.unwrap().name(), "Kimi Code (subscription)");
        assert_eq!(load_xai_oauth().await.unwrap().name(), "xAI (Grok/X subscription)");
        assert_eq!(load_cursor_oauth().await.unwrap().name(), "Cursor (Pro/Ultra/Teams)");
        assert_eq!(load_devin_oauth().await.unwrap().name(), "Devin");
    }
}
