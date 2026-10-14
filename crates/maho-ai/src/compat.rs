//! Port of senpi packages/ai/src/compat.ts: the registry-routed `stream`/`complete` API.
//!
//! `registerFauxProvider` lives in providers/faux. This module owns the builtin API table
//! (`BUILTIN_APIS`), applies the text tool-call middleware for `compat.toolCallFormat` models,
//! routes builtin models through their builtin provider (including Cloudflare's no-auth path) and
//! otherwise resolves the API provider from the registry and injects the provider env API key.

pub mod extension_oauth_types;

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use crate::api;
use crate::api_registry::{
    ApiProviderInternal, ApiRegistryError, clear_api_providers, get_api_provider, register_builtin_api_provider,
};
use crate::env_api_keys::get_env_api_key;
use crate::models::{Models, Provider};
use crate::tool_call_middleware::{get_protocol, get_tool_call_format, transform_context, wrap_stream_with_tool_call_middleware};
use crate::types::{
    AssistantMessage, AssistantMessageEventStream, Context, Model, ProviderRequestOptions, ProviderStreams, SimpleStreamOptions,
    StreamOptions,
};
use crate::utils::event_stream::StreamError;
use crate::utils::lazy::error_stream;

pub use crate::models_generated::{get_builtin_model as get_model, get_builtin_provider_models as get_models, get_builtin_providers as get_providers};

fn builtin_apis() -> Vec<(&'static str, Arc<dyn ProviderStreams>)> {
    vec![
        ("anthropic-messages", api::anthropic_messages_lazy::anthropic_messages_api()),
        ("openai-completions", api::openai_completions_lazy::open_ai_completions_api()),
        ("openai-responses", api::openai_responses_lazy::openai_responses_api()),
        ("openai-codex-responses", api::openai_codex_responses_lazy::openai_codex_responses_api()),
        ("cursor-agent", Arc::new(api::cursor_agent_lazy::cursor_agent_api())),
        ("devin-agent", api::devin_agent_lazy::devin_agent_api()),
        ("azure-openai-responses", api::azure_openai_responses_lazy::azure_openai_responses_api()),
        ("google-generative-ai", api::google_generative_ai_lazy::google_generative_ai_api()),
        ("google-vertex", api::google_vertex_lazy::google_vertex_api()),
        ("mistral-conversations", api::mistral_conversations_lazy::mistral_conversations_api()),
        ("bedrock-converse-stream", api::bedrock_converse_stream_lazy::bedrock_converse_stream_api()),
        ("pi-messages", api::pi_messages_lazy::pi_messages_api()),
    ]
}

static BUILTIN_API_PROVIDER_INSTANCES: LazyLock<Mutex<HashMap<String, ApiProviderInternal>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Registers the builtin API implementations into the api-registry without clobbering existing
/// entries: a test or extension may already have registered an override for a builtin api id.
/// Idempotent; the registry runs it once on first read (the TS import side effect). TS follows
/// `registerBuiltinApiProvider` with `if (!getApiProvider(api)) registerApiProvider(...)`, which
/// can never fire: the builtin registration already filled the unscoped registry, and a scoped
/// read falls back to the builtin registry.
pub fn register_builtin_api_providers() {
    for (api, streams) in builtin_apis() {
        let instance = register_builtin_api_provider(api, streams);
        BUILTIN_API_PROVIDER_INSTANCES.lock().unwrap_or_else(|p| p.into_inner()).insert(api.to_owned(), instance);
    }
}

pub fn reset_api_providers() -> Result<(), ApiRegistryError> {
    clear_api_providers()?;
    BUILTIN_API_PROVIDER_INSTANCES.lock().unwrap_or_else(|p| p.into_inner()).clear();
    register_builtin_api_providers();
    Ok(())
}

static COMPAT_MODELS: LazyLock<Models> = LazyLock::new(|| crate::providers::all::builtin_models(None));
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

fn has_resolved_cloudflare_auth(request: &ProviderRequestOptions) -> bool {
    has_explicit_api_key(request.api_key.as_deref())
        || request.headers.as_ref().is_some_and(|headers| matches!(headers.get("cf-aig-authorization"), Some(Some(_))))
}

fn get_builtin_provider_for_model(model: &Model) -> Result<Option<Arc<dyn Provider>>, ApiRegistryError> {
    let current = get_api_provider(&model.api)?;
    let is_builtin_instance = {
        let instances = BUILTIN_API_PROVIDER_INSTANCES.lock().unwrap_or_else(|p| p.into_inner());
        match (current.as_ref(), instances.get(&model.api)) {
            (Some(current), Some(builtin)) => Arc::ptr_eq(current.streams(), builtin.streams()),
            _ => false,
        }
    };
    if !is_builtin_instance {
        return Ok(None);
    }
    Ok(COMPAT_MODELS
        .get_provider(&model.provider)
        .filter(|provider| provider.get_models().iter().any(|candidate| candidate.api == model.api)))
}

fn resolve_api_provider(api: &str) -> Result<ApiProviderInternal, ApiRegistryError> {
    get_api_provider(api)?.ok_or_else(|| ApiRegistryError::NoProvider(api.to_owned()))
}

pub fn try_stream(model: &Model, context: &Context, options: Option<StreamOptions>) -> Result<AssistantMessageEventStream, ApiRegistryError> {
    if let Some(format) = get_tool_call_format(model)
        && let Some(tools) = context.tools.as_ref().filter(|tools| !tools.is_empty())
    {
        let protocol = get_protocol(format);
        let transformed_context = transform_context(context, protocol);
        let inner_stream = try_stream(model, &transformed_context, options)?;
        return Ok(wrap_stream_with_tool_call_middleware(inner_stream, protocol, tools.clone()));
    }

    if let Some(builtin_provider) = get_builtin_provider_for_model(model)? {
        let mut options = options.unwrap_or_default();
        if model.provider.starts_with("cloudflare-") && !has_resolved_cloudflare_auth(&options.request) {
            return Ok(COMPAT_MODELS.stream(model, context, Some(options), Default::default()));
        }
        with_env_api_key(model, &mut options.request);
        return Ok(builtin_provider.stream(model, context, Some(options)));
    }
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
    if let Some(format) = get_tool_call_format(model)
        && let Some(tools) = context.tools.as_ref().filter(|tools| !tools.is_empty())
    {
        let protocol = get_protocol(format);
        let transformed_context = transform_context(context, protocol);
        let inner_stream = try_stream_simple(model, &transformed_context, options)?;
        return Ok(wrap_stream_with_tool_call_middleware(inner_stream, protocol, tools.clone()));
    }

    if let Some(builtin_provider) = get_builtin_provider_for_model(model)? {
        let mut options = options.unwrap_or_default();
        if model.provider.starts_with("cloudflare-") && !has_resolved_cloudflare_auth(&options.stream.request) {
            return Ok(COMPAT_MODELS.stream_simple(model, context, Some(options), Default::default()));
        }
        with_env_api_key(model, &mut options.stream.request);
        return Ok(builtin_provider.stream_simple(model, context, Some(options)));
    }
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

    #[test]
    fn builtin_apis_register_on_first_read_and_reset_restores_them() {
        let scope = ProviderScope::new();
        let (before_reset, after_reset) = run_with_provider_scope(&scope, || {
            register_api_provider("anthropic-messages", Arc::new(KeyEcho), Some("override")).expect("register");
            let before = get_api_provider("anthropic-messages").expect("lookup").expect("provider");
            reset_api_providers().expect("reset");
            (before, get_api_provider("anthropic-messages").expect("lookup").expect("provider"))
        })
        .expect("scope");
        let builtin = crate::api_registry::get_builtin_api_provider("anthropic-messages").expect("builtin registered without CLI setup");
        assert!(!Arc::ptr_eq(before_reset.streams(), builtin.streams()));
        assert!(Arc::ptr_eq(after_reset.streams(), builtin.streams()));
        for (api, _) in builtin_apis() {
            assert!(crate::api_registry::get_builtin_api_provider(api).is_some(), "{api}");
        }
    }

    struct HermesEcho(Arc<std::sync::Mutex<Option<Context>>>);

    impl ProviderStreams for HermesEcho {
        fn stream(&self, model: &Model, context: &Context, _options: Option<StreamOptions>) -> AssistantMessageEventStream {
            *self.0.lock().expect("capture") = Some(context.clone());
            let text = r#"<tool_call>{"name":"get_weather","arguments":{"city":"Seoul"}}</tool_call>"#;
            let stream = AssistantMessageEventStream::assistant();
            let mut message = crate::utils::lazy::setup_error_message(model, "");
            message.error_message = None;
            message.stop_reason = StopReason::Stop;
            let start = message.clone();
            message.content = vec![crate::types::ContentBlock::Text(crate::types::TextContent { text: text.into(), ..Default::default() })];
            stream.push(AssistantMessageEvent::Start { partial: start.clone() });
            stream.push(AssistantMessageEvent::TextStart { content_index: 0, partial: start });
            stream.push(AssistantMessageEvent::TextDelta { content_index: 0, delta: text.into(), partial: message.clone() });
            stream.push(AssistantMessageEvent::TextEnd { content_index: 0, content: text.into(), partial: message.clone() });
            stream.push(AssistantMessageEvent::Done { reason: DoneReason::Stop, message });
            stream
        }

        fn stream_simple(&self, model: &Model, context: &Context, options: Option<SimpleStreamOptions>) -> AssistantMessageEventStream {
            self.stream(model, context, options.map(|o| o.stream))
        }
    }

    #[tokio::test]
    async fn tool_call_format_models_get_a_rewritten_context_and_parsed_tool_calls() {
        let scope = ProviderScope::new();
        let captured = Arc::new(std::sync::Mutex::new(None));
        let mut model = scoped_model("openai-completions");
        model.compat = Some(crate::model::ModelCompat(serde_json::Map::from_iter([("toolCallFormat".to_owned(), serde_json::json!("hermes"))])));
        let tool = crate::types::Tool {
            name: "get_weather".into(),
            description: "d".into(),
            parameters: serde_json::json!({"type": "object"}),
            freeform: None,
            constrained_sampling: None,
        };
        let context = Context { system_prompt: Some("base".into()), messages: Vec::new(), tools: Some(vec![tool]) };
        let wrapped = run_with_provider_scope(&scope, || {
            register_api_provider("openai-completions", Arc::new(HermesEcho(captured.clone())), None).expect("register");
            stream_simple(&model, &context, None)
        })
        .expect("scope");
        let message = wrapped.result().await.expect("result");
        let sent = captured.lock().expect("capture").clone().expect("provider called");
        assert!(sent.tools.is_none());
        assert!(sent.system_prompt.as_deref().is_some_and(|prompt| prompt.contains("<tool_call>") && prompt.ends_with("base")));
        assert_eq!(message.stop_reason, StopReason::ToolUse);
        assert!(message.content.iter().any(|block| matches!(block, crate::types::ContentBlock::ToolCall(call) if call.name == "get_weather")));
    }
}
