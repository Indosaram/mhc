//! Port of senpi packages/ai/src/legacy-api-aliases.ts.
//!
//! The TS module only re-exports the per-API `stream*`/`streamSimple*` functions of the wire APIs
//! (todos 10-12). Those lanes register their `ProviderStreams`; this module exposes the alias
//! names over the builtin API registry so callers resolve them without importing each wire module.

use crate::api_registry::{ApiRegistryError, get_api_provider};
use crate::types::{AssistantMessageEventStream, Context, Model, SimpleStreamOptions, StreamOptions};

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
    let base = alias.strip_prefix("streamSimple").map(|rest| format!("stream{rest}"));
    let name = base.as_deref().unwrap_or(alias);
    LEGACY_API_ALIASES.iter().find(|(n, _)| *n == name).map(|(_, api)| *api)
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
    let api = api_for_alias(alias).ok_or_else(|| LegacyAliasError::UnknownAlias(alias.into()))?;
    let provider = get_api_provider(api)?.ok_or_else(|| ApiRegistryError::NoProvider(api.into()))?;
    Ok(provider.stream(model, context, options))
}

pub fn stream_simple_by_alias(
    alias: &str,
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
) -> Result<AssistantMessageEventStream, LegacyAliasError> {
    let api = api_for_alias(alias).ok_or_else(|| LegacyAliasError::UnknownAlias(alias.into()))?;
    let provider = get_api_provider(api)?.ok_or_else(|| ApiRegistryError::NoProvider(api.into()))?;
    Ok(provider.stream_simple(model, context, options))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_stream_and_stream_simple_aliases() {
        assert_eq!(api_for_alias("streamAnthropic"), Some("anthropic-messages"));
        assert_eq!(api_for_alias("streamSimpleOpenAIResponses"), Some("openai-responses"));
        assert_eq!(api_for_alias("streamNope"), None);
    }
}
