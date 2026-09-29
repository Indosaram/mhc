//! Port of senpi packages/ai/src/compat.ts: the registry-routed `stream`/`complete` API.
//!
//! Tool-call middleware wrapping (tool_call_middleware/), builtin provider routing and the builtin
//! API registrations (api/*.lazy, providers/all) and `registerFauxProvider` (providers/faux) are
//! filled in by todos 10-13; this module resolves the API provider from the registry and injects
//! the provider env API key exactly as the TS fallback path does.

pub mod extension_oauth_types;

use crate::api_registry::{ApiRegistryError, get_api_provider};
use crate::env_api_keys::get_env_api_key;
use crate::types::{AssistantMessage, AssistantMessageEventStream, Context, Model, ProviderRequestOptions, SimpleStreamOptions, StreamOptions};
use crate::utils::event_stream::StreamError;
use crate::utils::lazy::error_stream;

pub use crate::models_generated::{get_builtin_model as get_model, get_builtin_provider_models as get_models, get_builtin_providers as get_providers};

const AMBIENT_AUTH_MARKER: &str = "<authenticated>";

fn has_explicit_api_key(api_key: Option<&str>) -> bool {
    api_key.is_some_and(|key| !crate::utils::js::trim(key).is_empty())
}

fn with_env_api_key(model: &Model, request: &mut ProviderRequestOptions) {
    if has_explicit_api_key(request.api_key.as_deref()) {
        return;
    }
    if let Some(api_key) = get_env_api_key(&model.provider, request.env.as_ref()).filter(|key| key != AMBIENT_AUTH_MARKER) {
        request.api_key = Some(api_key);
    }
}

fn resolve_api_provider(api: &str) -> Result<crate::api_registry::ApiProviderInternal, ApiRegistryError> {
    get_api_provider(api)?.ok_or_else(|| ApiRegistryError::NoProvider(api.to_owned()))
}

pub fn try_stream(model: &Model, context: &Context, options: Option<StreamOptions>) -> Result<AssistantMessageEventStream, ApiRegistryError> {
    let provider = resolve_api_provider(&model.api)?;
    let mut options = options.unwrap_or_default();
    with_env_api_key(model, &mut options.request);
    Ok(provider.stream(model, context, Some(options)))
}

pub fn try_stream_simple(
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
) -> Result<AssistantMessageEventStream, ApiRegistryError> {
    let provider = resolve_api_provider(&model.api)?;
    let mut options = options.unwrap_or_default();
    with_env_api_key(model, &mut options.stream.request);
    Ok(provider.stream_simple(model, context, Some(options)))
}

/// Like TS `stream`, whose synchronous throw a caller sees as a failed stream.
pub fn stream(model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
    try_stream(model, context, options).unwrap_or_else(|error| error_stream(model, &error.to_string()))
}

pub fn stream_simple(model: &Model, context: &Context, options: Option<SimpleStreamOptions>) -> AssistantMessageEventStream {
    try_stream_simple(model, context, options).unwrap_or_else(|error| error_stream(model, &error.to_string()))
}

pub async fn complete(model: &Model, context: &Context, options: Option<StreamOptions>) -> Result<AssistantMessage, StreamError> {
    stream(model, context, options).result().await
}

pub async fn complete_simple(model: &Model, context: &Context, options: Option<SimpleStreamOptions>) -> Result<AssistantMessage, StreamError> {
    stream_simple(model, context, options).result().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_registry::register_api_provider;
    use crate::node::provider_scope::{ProviderScope, run_with_provider_scope};
    use crate::types::{AssistantMessageEvent, DoneReason, ProviderStreams, StopReason};
    use std::sync::Arc;

    struct KeyEcho;

    impl ProviderStreams for KeyEcho {
        fn stream(&self, model: &Model, _context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
            let stream = AssistantMessageEventStream::assistant();
            let mut message = crate::utils::lazy::setup_error_message(model, "");
            message.stop_reason = StopReason::Stop;
            message.error_message = None;
            message.response_id = options.and_then(|o| o.request.api_key);
            stream.push(AssistantMessageEvent::Done { reason: DoneReason::Stop, message });
            stream
        }

        fn stream_simple(&self, model: &Model, context: &Context, options: Option<SimpleStreamOptions>) -> AssistantMessageEventStream {
            self.stream(model, context, options.map(|o| o.stream))
        }
    }

    fn scoped_model(api: &str) -> Model {
        let mut model = crate::models_generated::get_builtin_model("anthropic", "claude-opus-4-8").expect("model").clone();
        model.api = api.into();
        model
    }

    #[tokio::test]
    async fn routes_through_the_scoped_registry_and_injects_env_keys() {
        let scope = ProviderScope::new();
        let model = scoped_model("maho-compat-test");
        let (with_env, explicit, missing) = run_with_provider_scope(&scope, || {
            register_api_provider("maho-compat-test", Arc::new(KeyEcho), None).expect("register");
            let mut env_options = StreamOptions::default();
            env_options.request.env = Some([("ANTHROPIC_API_KEY".to_owned(), "env-key".to_owned())].into_iter().collect());
            let mut explicit_options = env_options.clone();
            explicit_options.request.api_key = Some("explicit".into());
            (
                stream(&model, &Context::default(), Some(env_options)),
                stream(&model, &Context::default(), Some(explicit_options)),
                try_stream(&scoped_model("maho-missing-api"), &Context::default(), None).err(),
            )
        })
        .expect("scope");
        assert_eq!(with_env.result().await.expect("result").response_id.as_deref(), Some("env-key"));
        assert_eq!(explicit.result().await.expect("result").response_id.as_deref(), Some("explicit"));
        assert_eq!(missing, Some(ApiRegistryError::NoProvider("maho-missing-api".into())));
        assert_eq!(missing.map(|e| e.to_string()).as_deref(), Some("No API provider registered for api: maho-missing-api"));
    }

    #[tokio::test]
    async fn unregistered_api_yields_an_error_terminated_stream() {
        let scope = ProviderScope::new();
        let model = scoped_model("maho-none");
        let failed = run_with_provider_scope(&scope, || stream(&model, &Context::default(), None)).expect("scope");
        let message = failed.result().await.expect("result");
        assert_eq!(message.stop_reason, StopReason::Error);
        assert_eq!(message.error_message.as_deref(), Some("No API provider registered for api: maho-none"));
    }
}
