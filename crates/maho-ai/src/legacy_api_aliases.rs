//! Port of senpi packages/ai/src/legacy-api-aliases.ts.
//!
//! Legacy names capture the builtin wire implementations, independent of registry mutations.

use crate::api_registry::ApiRegistryError;
use crate::types::{
    AssistantMessageEventStream, Context, Model, ProviderStreams, SimpleStreamOptions,
    StreamOptions,
};
use std::sync::{Arc, LazyLock};

static LEGACY_STREAMS: LazyLock<[Arc<dyn ProviderStreams>; 8]> = LazyLock::new(|| {
    use crate::api;
    [
        api::anthropic_messages_lazy::anthropic_messages_api(),
        api::azure_openai_responses_lazy::azure_openai_responses_api(),
        api::google_generative_ai_lazy::google_generative_ai_api(),
        api::google_vertex_lazy::google_vertex_api(),
        api::mistral_conversations_lazy::mistral_conversations_api(),
        api::openai_codex_responses_lazy::openai_codex_responses_api(),
        api::openai_completions_lazy::open_ai_completions_api(),
        api::openai_responses_lazy::openai_responses_api(),
    ]
});

/// Legacy alias name -> API id, as exported by senpi.
pub const LEGACY_API_ALIASES: &[(&str, &str)] = &[
    ("streamAnthropic", "anthropic-messages"),
    ("streamAzureOpenAIResponses", "azure-openai-responses"),
    ("streamGoogle", "google-generative-ai"),
    ("streamGoogleVertex", "google-vertex"),
    ("streamMistral", "mistral-conversations"),
    ("streamOpenAICodexResponses", "openai-codex-responses"),
    ("streamOpenAICompletions", "openai-completions"),
    ("streamOpenAIResponses", "openai-responses"),
];

fn api_for_alias(alias: &str) -> Option<&'static str> {
    let base = alias
        .strip_prefix("streamSimple")
        .map(|rest| format!("stream{rest}"));
    let name = base.as_deref().unwrap_or(alias);
    LEGACY_API_ALIASES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, api)| *api)
}

fn streams_for_alias(alias: &str) -> Result<&'static Arc<dyn ProviderStreams>, LegacyAliasError> {
    let api = api_for_alias(alias).ok_or_else(|| LegacyAliasError::UnknownAlias(alias.into()))?;
    LEGACY_API_ALIASES
        .iter()
        .zip(LEGACY_STREAMS.iter())
        .find_map(|((_, id), streams)| (*id == api).then_some(streams))
        .ok_or_else(|| LegacyAliasError::UnknownAlias(alias.into()))
}

#[derive(Debug, thiserror::Error)]
pub enum LegacyAliasError {
    #[error("Unknown legacy API alias: {0}")]
    UnknownAlias(String),
    #[error(transparent)]
    Registry(#[from] ApiRegistryError),
}

pub fn stream_by_alias(
    alias: &str,
    model: &Model,
    context: &Context,
    options: Option<StreamOptions>,
) -> Result<AssistantMessageEventStream, LegacyAliasError> {
    let provider = streams_for_alias(alias)?;
    Ok(provider.stream(model, context, options))
}

pub fn stream_simple_by_alias(
    alias: &str,
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
) -> Result<AssistantMessageEventStream, LegacyAliasError> {
    let provider = streams_for_alias(alias)?;
    Ok(provider.stream_simple(model, context, options))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_stream_and_stream_simple_aliases() {
        assert_eq!(api_for_alias("streamAnthropic"), Some("anthropic-messages"));
        assert_eq!(
            api_for_alias("streamSimpleOpenAIResponses"),
            Some("openai-responses")
        );
        assert_eq!(api_for_alias("streamNope"), None);
    }

    #[test]
    fn captured_aliases_ignore_scoped_overrides_and_closed_scopes() {
        use crate::api_registry::{clear_api_providers, register_api_provider};
        use crate::node::provider_scope::{ProviderScope, run_with_provider_scope};
        let scope = ProviderScope::new();
        let captured = streams_for_alias("streamAnthropic").expect("alias").clone();
        run_with_provider_scope(&scope, || {
            let replacement = crate::api::openai_responses_lazy::openai_responses_api();
            register_api_provider("anthropic-messages", replacement.clone(), None)
                .expect("override");
            let alias = streams_for_alias("streamAnthropic").expect("alias");
            assert!(!Arc::ptr_eq(alias, &replacement));
            assert!(Arc::ptr_eq(alias, &captured));
            clear_api_providers().expect("clear");
            scope.close();
            assert!(Arc::ptr_eq(
                streams_for_alias("streamSimpleAnthropic").expect("closed alias"),
                &captured
            ));
        })
        .expect("scope");
    }

    #[tokio::test]
    async fn public_aliases_stream_through_a_closed_scope() {
        use crate::node::provider_scope::{ProviderScope, run_with_provider_scope};
        let scope = ProviderScope::new();
        let model = crate::models_generated::get_builtin_provider_models("anthropic")
            .expect("models")
            .into_iter()
            .next()
            .expect("model");
        let context = Context::default();
        let streams = run_with_provider_scope(&scope, || {
            scope.close();
            (
                stream_by_alias("streamAnthropic", model, &context, None).expect("stream alias"),
                stream_simple_by_alias("streamSimpleAnthropic", model, &context, None)
                    .expect("simple alias"),
            )
        })
        .expect("scope");

        for stream in [streams.0, streams.1] {
            let message = tokio::time::timeout(std::time::Duration::from_secs(5), stream.result())
                .await
                .expect("bounded result")
                .expect("result");
            assert_eq!(message.stop_reason, crate::types::StopReason::Error);
            assert_eq!(
                message.error_message,
                Some(format!("No API key for provider: {}", model.provider))
            );
        }
    }
}
