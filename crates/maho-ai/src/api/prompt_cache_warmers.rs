use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};

use futures::future::BoxFuture;
use serde_json::Value;

use crate::types::{Context, Model, SimpleStreamOptions, Usage};

pub type PromptCacheWarmer = Arc<
    dyn Fn(
            Model,
            Context,
            SimpleStreamOptions,
        ) -> BoxFuture<'static, Result<(Usage, Value), String>>
        + Send
        + Sync,
>;

static WARMERS: LazyLock<RwLock<HashMap<String, PromptCacheWarmer>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

pub fn register_prompt_cache_warmer(api: &str, warmer: PromptCacheWarmer) {
    WARMERS
        .write()
        .unwrap_or_else(|error| error.into_inner())
        .insert(api.to_owned(), warmer);
}

pub fn get_prompt_cache_warmer(api: &str) -> Option<PromptCacheWarmer> {
    WARMERS
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .get(api)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registered_warmer_receives_simple_options() {
        let api = "audit-resp2-registry-options";
        register_prompt_cache_warmer(
            api,
            Arc::new(|_, _, options| {
                Box::pin(async move {
                    Ok((
                        Usage::default(),
                        serde_json::json!({"reasoning": options.reasoning, "session": options.stream.session_id}),
                    ))
                })
            }),
        );
        let model = crate::models_generated::get_builtin_provider_models("openai").expect("models")
            [0]
        .clone();
        let options = SimpleStreamOptions {
            reasoning: Some(crate::types::ThinkingLevel::High),
            stream: crate::types::StreamOptions {
                session_id: Some("warm-session".into()),
                ..crate::types::StreamOptions::default()
            },
            ..SimpleStreamOptions::default()
        };
        let (_, raw) =
            get_prompt_cache_warmer(api).expect("registered")(model, Context::default(), options)
                .await
                .expect("warm");
        assert_eq!(
            raw,
            serde_json::json!({"reasoning":"high", "session":"warm-session"})
        );
    }
}
