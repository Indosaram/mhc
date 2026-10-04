//! Ports of the senpi packages/ai provider test cases that need only providers/ plus the merged
//! todo-5 code. Cases that need another node's module are listed in the node rows file.

use super::all::{
    builtin_images_models, builtin_images_providers, builtin_models, builtin_providers, get_builtin_model,
    get_builtin_model_data_generated_at, get_builtin_models, get_builtin_providers,
};
use super::cloudflare_stream::resolve_cloudflare_model;
use super::faux::{
    FauxAssistantMessageOptions, FauxProviderState, FauxResponseStep, RegisterFauxProviderOptions, create_faux_core,
    faux_assistant_message, faux_streams, faux_text, faux_thinking, faux_tool_call, register_faux_provider,
};
use super::opencode_headers::with_open_code_session_header;
use crate::models_generated::MODELS;
use crate::models::{CreateProviderOptions, ModelsRequestTransforms, ProviderApi, create_models, create_provider};
use crate::types::{
    AssistantMessageEvent, BoxFuture, CacheRetention, Context, ContentBlock, DeferredCancelOptions, DeferredHandle,
    DeferredOption, Message, Model, ModelThinkingLevel, ProviderEnv, ProviderRequestOptions, ProviderStreams,
    SimpleStreamOptions, StopReason, StreamOptions, UserContent, UserMessage,
};
use serde_json::{Map, json};
use std::sync::{Arc, Mutex};

fn user_message(text: &str) -> Message {
    Message::User(UserMessage { content: UserContent::Text(text.to_owned()), timestamp: 0 })
}

fn simple_context() -> Context {
    Context { system_prompt: None, messages: vec![user_message("hi")], tools: None }
}

/// The TS \`fauxProvider()\` builds its provider with \`auth: { apiKey: { resolve: async () => ({ auth: {} }) } }\`;
/// the Rust seam for that is a \`ModelsAuth\` that resolves without a credential.
struct FauxModelsAuth;

impl crate::models::ModelsAuth for FauxModelsAuth {
    fn resolve<'a>(
        &'a self,
        _provider: &'a dyn crate::models::Provider,
        _overrides: &'a crate::models::AuthResolutionOverrides,
    ) -> crate::types::BoxFuture<'a, Result<Option<crate::models::AuthResolution>, crate::models::ModelsError>> {
        Box::pin(async { Ok(Some(crate::models::AuthResolution::default())) })
    }

    fn refresh_credential<'a>(
        &'a self,
        _provider: &'a dyn crate::models::Provider,
        _stored: Option<&'a crate::models::Credential>,
        _signal: &'a crate::utils::abort::AbortSignal,
    ) -> crate::types::BoxFuture<'a, Result<Option<crate::models::Credential>, crate::models::ModelsError>> {
        Box::pin(async { Ok(None) })
    }
}

fn faux_models() -> crate::models::Models {
    crate::models::create_models(Some(crate::models::CreateModelsOptions {
        models_store: None,
        auth: Some(Arc::new(FauxModelsAuth)),
    }))
}

#[test]
fn builtin_models_registers_every_builtin_provider_with_models() {
    let models = builtin_models(None);
    let providers = models.get_providers();
    assert_eq!(providers.len(), builtin_providers().len());
    let ids: Vec<&str> = providers.iter().map(|provider| provider.id()).collect();
    for id in ["anthropic", "bai", "ollama"] {
        assert!(ids.contains(&id), "missing builtin provider {id}");
    }

    let anthropic = models.get_model("anthropic", "claude-haiku-4-5").expect("anthropic model");
    assert_eq!(anthropic.api, "anthropic-messages");
    assert!(models.get_models(None).len() > 500);

    // Static providers list models immediately; Radius, Ollama, B.AI and Cursor discover
    // account-specific availability only after authentication.
    for provider in providers {
        let list = models.get_models(Some(provider.id()));
        if matches!(provider.id(), "radius" | "ollama" | "bai" | "cursor") {
            assert!(list.is_empty(), "{} should ship no static catalog", provider.id());
        } else {
            assert!(!list.is_empty(), "{} should ship a catalog", provider.id());
        }
        assert!(list.iter().all(|model| model.provider == provider.id()));
    }
}

#[test]
fn ships_bai_standard_metadata_while_leaving_availability_credential_scoped() {
    let model = get_builtin_model("bai", "gpt-5.6-sol").expect("bai flagship");
    assert_eq!(model.api, "openai-responses");
    assert_eq!(model.provider, "bai");
    assert_eq!(model.base_url, "https://api.b.ai/v1");
    assert!(model.reasoning);
    assert_eq!(model.cost.input, 4.0);
    assert_eq!(model.cost.output, 20.0);
    assert_eq!(model.cost.cache_read, 0.4);
    assert_eq!(model.cost.cache_write, 5.0);
    assert_eq!(model.context_window, 922_000);
    assert_eq!(model.max_tokens, 128_000);

    let anthropic = get_builtin_model("bai", "claude-sonnet-5").expect("bai anthropic entry");
    assert_eq!(anthropic.api, "anthropic-messages");
    assert_eq!(anthropic.base_url, "https://api.b.ai");

    assert_eq!(get_builtin_models("bai").len(), 56);
    assert!(!get_builtin_models("bai").iter().any(|model| model.id == "gpt-image-2"));
}

#[test]
fn stores_native_constrained_sampling_capabilities_in_model_metadata() {
    let gpt_4o = get_builtin_model("openai", "gpt-4o").expect("gpt-4o");
    let compat = gpt_4o.compat.expect("compat");
    assert_eq!(compat.get("supportsStrictMode"), Some(&json!(true)));

    let gpt_5_4 = get_builtin_model("openai", "gpt-5.4").expect("gpt-5.4");
    let compat = gpt_5_4.compat.expect("compat");
    assert_eq!(compat.get("supportsStrictMode"), Some(&json!(true)));

    let anthropic = get_builtin_model("anthropic", "claude-haiku-4-5").expect("anthropic");
    assert_eq!(anthropic.compat.expect("compat").get("supportsStrictTools"), Some(&json!(true)));
}

#[test]
fn uses_official_kimi_k3_pricing_for_moonshot_providers() {
    for provider in ["moonshotai", "moonshotai-cn"] {
        let model = get_builtin_model(provider, "kimi-k3").expect("kimi-k3");
        assert_eq!(model.cost.input, 3.0);
        assert_eq!(model.cost.output, 15.0);
        assert_eq!(model.cost.cache_read, 0.3);
        assert_eq!(model.cost.cache_write, 0.0);
    }
}

#[test]
fn uses_api_equivalent_implied_pricing_for_kimi_coding_subscription_models() {
    let expected: [(&str, (f64, f64, f64)); 3] = [
        ("kimi-for-coding", (0.95, 4.0, 0.19)),
        ("k3", (3.0, 15.0, 0.3)),
        ("kimi-for-coding-highspeed", (1.9, 8.0, 0.38)),
    ];
    for (model_id, cost) in expected {
        let model = get_builtin_model("kimi-coding", model_id).expect("kimi-coding model");
        assert_eq!((model.cost.input, model.cost.output, model.cost.cache_read), cost, "{model_id}");
    }
}

#[test]
fn keeps_a_fork_owned_provider_out_of_the_generated_aggregate() {
    assert!(!MODELS.contains_key("kimi-coding"));
    assert!(get_builtin_providers().contains(&"kimi-coding"));
    let model = get_builtin_model("kimi-coding", "kimi-for-coding").expect("kimi-for-coding");
    assert_eq!(model.provider, "kimi-coding");
    assert_eq!(model.api, "anthropic-messages");
    let models = get_builtin_models("kimi-coding");
    assert!(!models.is_empty());
    assert!(models.iter().all(|model| model.provider == "kimi-coding"));
}

#[test]
fn normalizes_the_anthropic_model_thinking_level_map() {
    let model = get_builtin_model("anthropic", "claude-opus-4-8").expect("anthropic model");
    let map = model.thinking_level_map.expect("thinking level map");
    assert_eq!(map.get(&ModelThinkingLevel::Max), Some(&Some("max".to_owned())));
    assert_eq!(get_builtin_model_data_generated_at(), Some(1_790_168_420_361));
}

#[test]
fn lists_every_builtin_images_provider() {
    let providers = builtin_images_providers();
    let ids: Vec<&str> = providers.iter().map(|provider| provider.id()).collect();
    assert_eq!(ids, ["openrouter", "openai"]);
    let models = builtin_images_models();
    assert_eq!(models.get_providers().len(), 2);
    assert!(!models.get_models(Some("openai")).is_empty());
    assert!(!models.get_models(Some("openrouter")).is_empty());
}

#[test]
fn resolves_cloudflare_model_placeholders_from_provider_env() {
    let model = get_builtin_model("cloudflare-ai-gateway", "workers-ai/@cf/meta/llama-3.3-70b-instruct-fp8-fast")
        .expect("workers-ai gateway entry");
    assert_eq!(resolve_cloudflare_model(&model, None), model);

    let mut env = ProviderEnv::new();
    env.insert("CLOUDFLARE_ACCOUNT_ID".to_owned(), "account-id".to_owned());
    env.insert("CLOUDFLARE_GATEWAY_ID".to_owned(), "gateway-id".to_owned());
    let resolved = resolve_cloudflare_model(&model, Some(&env));
    assert_eq!(
        resolved.base_url,
        "https://gateway.ai.cloudflare.com/v1/account-id/gateway-id/compat"
    );
}

struct EchoStreams;

impl ProviderStreams for EchoStreams {
    fn stream(&self, model: &Model, _context: &Context, options: Option<StreamOptions>) -> crate::types::AssistantMessageEventStream {
        let stream = crate::utils::event_stream::create_assistant_message_event_stream();
        let mut message = faux_assistant_message("ok", FauxAssistantMessageOptions::default());
        message.model = model.id.clone();
        message.response_id = options.and_then(|options| options.request.headers).and_then(|headers| {
            headers.iter().find(|(key, _)| key.to_lowercase() == "x-opencode-session").and_then(|(_, value)| value.clone())
        });
        stream.push(AssistantMessageEvent::Done { reason: crate::types::DoneReason::Stop, message: message.clone() });
        stream.end(Some(message));
        stream
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> crate::types::AssistantMessageEventStream {
        self.stream(model, context, options.map(|options| options.stream))
    }
}

#[tokio::test]
async fn adds_the_opencode_session_header_before_dispatch() {
    let model = get_builtin_model("opencode", "claude-sonnet-5").expect("opencode model");
    let wrapped = with_open_code_session_header(Arc::new(EchoStreams));

    let mut options = StreamOptions { session_id: Some("session-1".to_owned()), ..StreamOptions::default() };
    let message = wrapped.stream(&model, &simple_context(), Some(options.clone())).result().await.expect("result");
    assert_eq!(message.response_id.as_deref(), Some("session-1"));

    options.session_id = None;
    let message = wrapped.stream(&model, &simple_context(), Some(options)).result().await.expect("result");
    assert_eq!(message.response_id, None);

    let mut headers = Map::new();
    headers.insert("X-OpenCode-Session".to_owned(), json!("pinned"));
    let mut options = StreamOptions {
        session_id: Some("session-2".to_owned()),
        ..StreamOptions::default()
    };
    options.request.headers = Some([("X-OpenCode-Session".to_owned(), Some("pinned".to_owned()))].into_iter().collect());
    let message = wrapped.stream(&model, &simple_context(), Some(options)).result().await.expect("result");
    assert_eq!(message.response_id.as_deref(), Some("pinned"));
}

fn fixed_chunk_options() -> RegisterFauxProviderOptions {
    RegisterFauxProviderOptions {
        token_size: Some(super::faux::FauxTokenSize { min: Some(1), max: Some(1) }),
        ..RegisterFauxProviderOptions::default()
    }
}

#[tokio::test]
async fn faux_streams_queued_responses_through_a_models_collection() {
    let faux = super::faux::faux_provider(RegisterFauxProviderOptions::default());
    let models = faux_models();
    models.set_provider(faux.provider.clone());
    faux.set_responses(vec![faux_assistant_message("hello from faux", FauxAssistantMessageOptions::default()).into()]);

    let model = models.get_models(Some(faux.provider.id())).into_iter().next().expect("faux model");
    let result = models.complete_simple(&model, &simple_context(), None, Default::default()).await.expect("result");
    assert_eq!(result.stop_reason, StopReason::Stop);
    assert_eq!(result.content, vec![ContentBlock::Text(crate::types::TextContent { text: "hello from faux".to_owned(), ..Default::default() })]);
    assert_eq!(faux.state().call_count, 1);
}

#[tokio::test]
async fn faux_streams_an_exact_event_order_for_fixed_size_chunks() {
    let core = create_faux_core(&fixed_chunk_options());
    let message = faux_assistant_message(
        vec![faux_thinking("go"), faux_text("ok"), faux_tool_call("echo", Map::new(), Some("tool-1"))],
        FauxAssistantMessageOptions { stop_reason: Some(StopReason::ToolUse), ..Default::default() },
    );
    core.set_responses(vec![message.into()]);

    let model = core.get_model(None).expect("faux model");
    let events = faux_streams(core).stream(&model, &simple_context(), None).collect().await.expect("events");
    let types: Vec<&str> = events.iter().map(|event| event_kind(event)).collect();
    assert_eq!(
        types,
        [
            "start",
            "thinking_start",
            "thinking_delta",
            "thinking_end",
            "text_start",
            "text_delta",
            "text_end",
            "toolcall_start",
            "toolcall_delta",
            "toolcall_end",
            "done",
        ]
    );
    match &events[0] {
        AssistantMessageEvent::Start { partial } => assert_eq!(partial.stop_reason, StopReason::Pending),
        other => panic!("unexpected first event: {other:?}"),
    }
}

fn event_kind(event: &AssistantMessageEvent) -> &'static str {
    match event {
        AssistantMessageEvent::Start { .. } => "start",
        AssistantMessageEvent::TextStart { .. } => "text_start",
        AssistantMessageEvent::TextDelta { .. } => "text_delta",
        AssistantMessageEvent::TextEnd { .. } => "text_end",
        AssistantMessageEvent::ThinkingStart { .. } => "thinking_start",
        AssistantMessageEvent::ThinkingDelta { .. } => "thinking_delta",
        AssistantMessageEvent::ThinkingEnd { .. } => "thinking_end",
        AssistantMessageEvent::ToolcallStart { .. } => "toolcall_start",
        AssistantMessageEvent::ToolcallDelta { .. } => "toolcall_delta",
        AssistantMessageEvent::ToolcallEnd { .. } => "toolcall_end",
        AssistantMessageEvent::Done { .. } => "done",
        AssistantMessageEvent::Error { .. } => "error",
    }
}

#[tokio::test]
async fn faux_simulates_prompt_caching_per_session_id() {
    let core = create_faux_core(&RegisterFauxProviderOptions::default());
    core.set_responses(vec![
        faux_assistant_message("first", FauxAssistantMessageOptions::default()).into(),
        faux_assistant_message("second", FauxAssistantMessageOptions::default()).into(),
    ]);
    let model = core.get_model(None).expect("faux model");
    let streams = faux_streams(core.clone());

    let mut context = Context {
        system_prompt: Some("Be concise.".to_owned()),
        messages: vec![user_message("hello")],
        tools: None,
    };
    let options = StreamOptions {
        session_id: Some("session-1".to_owned()),
        cache_retention: Some(CacheRetention::Short),
        ..StreamOptions::default()
    };
    let first = streams.stream(&model, &context, Some(options.clone())).result().await.expect("first");
    assert_eq!(first.usage.cache_read, 0);
    assert!(first.usage.cache_write > 0);

    context.messages.push(Message::Assistant(Box::new(first)));
    context.messages.push(user_message("follow up"));

    let second = streams.stream(&model, &context, Some(options)).result().await.expect("second");
    assert!(second.usage.cache_read > 0);
    assert!(second.usage.input + second.usage.cache_read > second.usage.input);
}

#[tokio::test]
async fn faux_does_not_simulate_caching_when_cache_retention_is_none() {
    let core = create_faux_core(&RegisterFauxProviderOptions::default());
    core.set_responses(vec![
        faux_assistant_message("first", FauxAssistantMessageOptions::default()).into(),
        faux_assistant_message("second", FauxAssistantMessageOptions::default()).into(),
    ]);
    let model = core.get_model(None).expect("faux model");
    let streams = faux_streams(core.clone());

    let mut context = Context { system_prompt: None, messages: vec![user_message("hello")], tools: None };
    let options = StreamOptions {
        session_id: Some("session-1".to_owned()),
        cache_retention: Some(CacheRetention::None),
        ..StreamOptions::default()
    };
    streams.stream(&model, &context, Some(options.clone())).result().await.expect("first");
    context.messages.push(user_message("follow up"));
    let second = streams.stream(&model, &context, Some(options)).result().await.expect("second");
    assert_eq!(second.usage.cache_read, 0);
    assert_eq!(second.usage.cache_write, 0);
}

#[tokio::test]
async fn faux_emits_an_error_when_a_response_factory_throws() {
    let core = create_faux_core(&RegisterFauxProviderOptions::default());
    core.set_responses(vec![FauxResponseStep::Factory(Arc::new(|_context, _options, _state, _model| {
        Box::pin(async { panic!("boom") })
    }))]);
    let model = core.get_model(None).expect("faux model");
    let events = faux_streams(core).stream(&model, &simple_context(), None).collect().await.expect("events");
    assert_eq!(events.len(), 1);
    match &events[0] {
        AssistantMessageEvent::Error { error, .. } => {
            assert_eq!(error.stop_reason, StopReason::Error);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn faux_registers_and_unregisters_an_api_provider() {
    let registration = register_faux_provider(RegisterFauxProviderOptions::default());
    registration.set_responses(vec![faux_assistant_message("hello", FauxAssistantMessageOptions::default()).into()]);
    let model = registration.get_model(None).expect("faux model");
    let message = crate::compat::complete(&model, &simple_context(), None).await.expect("result");
    assert_eq!(message.stop_reason, StopReason::Stop);
    registration.unregister();
    let failed = crate::compat::stream(&model, &simple_context(), None).result().await.expect("result");
    assert_eq!(failed.stop_reason, StopReason::Error);
    assert_eq!(failed.error_message.as_deref(), Some(&*format!("No API provider registered for api: {}", registration.api)));
}

#[tokio::test]
async fn faux_records_call_log_and_pending_responses() {
    let core = create_faux_core(&RegisterFauxProviderOptions::default());
    assert_eq!(core.get_pending_response_count(), 0);
    core.set_responses(vec![faux_assistant_message("one", FauxAssistantMessageOptions::default()).into()]);
    core.append_responses(vec![faux_assistant_message("two", FauxAssistantMessageOptions::default()).into()]);
    assert_eq!(core.get_pending_response_count(), 2);

    let model = core.get_model(None).expect("faux model");
    faux_streams(core.clone()).stream(&model, &simple_context(), None).result().await.expect("result");
    assert_eq!(core.get_pending_response_count(), 1);
    let log = core.get_call_log();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].model_id, model.id);
    let state: FauxProviderState = core.state();
    assert_eq!(state.call_count, 1);
}

/// Pinned `providers.test.ts` "applies resolved request options to deferred fetch and cancellation":
/// `Models.cancelDeferred` resolves the provider, runs `applyAuth`, and hands the provider the
/// request model plus the resolved options (apiKey/headers/env/transformHeaders).
struct CancelRecordingStreams {
    calls: Mutex<Vec<(Model, DeferredHandle, Option<DeferredCancelOptions>)>>,
}

impl ProviderStreams for CancelRecordingStreams {
    fn stream(&self, model: &Model, _context: &Context, _options: Option<StreamOptions>) -> crate::types::AssistantMessageEventStream {
        let stream = crate::utils::event_stream::create_assistant_message_event_stream();
        let mut message = faux_assistant_message("ok", FauxAssistantMessageOptions::default());
        message.model = model.id.clone();
        stream.push(AssistantMessageEvent::Done { reason: crate::types::DoneReason::Stop, message: message.clone() });
        stream.end(Some(message));
        stream
    }

    fn stream_simple(&self, model: &Model, context: &Context, options: Option<SimpleStreamOptions>) -> crate::types::AssistantMessageEventStream {
        self.stream(model, context, options.map(|options| options.stream))
    }

    fn cancel_deferred<'a>(
        &'a self,
        model: &'a Model,
        handle: &'a DeferredHandle,
        options: Option<DeferredCancelOptions>,
    ) -> BoxFuture<'a, Result<(), String>> {
        let recorded = (model.clone(), handle.clone(), options);
        Box::pin(async move {
            self.calls.lock().expect("cancel calls").push(recorded);
            Ok(())
        })
    }

    fn supports_cancel_deferred(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn provider_cancel_deferred_receives_the_resolved_model_and_request_options() {
    let base = get_builtin_model("openai", "").expect("openai model");
    let mut model = base.clone();
    model.provider = "cancel-recording".into();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let models = create_models(None);
    models.set_provider(create_provider(CreateProviderOptions {
        id: "cancel-recording".into(),
        name: None,
        base_url: None,
        headers: None,
        models: vec![model.clone()],
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(Arc::new(CancelRecordingStreams { calls: calls.clone() })),
    }));
    let handle = DeferredHandle {
        provider: model.provider.clone(),
        model_id: model.id.clone(),
        api: model.api.clone(),
        id: "response-1".into(),
        expires_at: None,
        poll_after_ms: None,
        data: None,
    };
    let options = ProviderRequestOptions {
        api_key: Some("request-key".into()),
        timeout_ms: Some(200),
        ..ProviderRequestOptions::default()
    };

    models.cancel_deferred(&model, &handle, Some(options)).await.expect("cancel forwards to the provider");

    let calls = calls.lock().expect("cancel calls");
    assert_eq!(calls.len(), 1);
    let (forwarded_model, forwarded_handle, forwarded_options) = &calls[0];
    assert_eq!(forwarded_model.id, model.id);
    assert_eq!(forwarded_handle.id, "response-1");
    let forwarded_options = forwarded_options.as_ref().expect("resolved options");
    assert_eq!(forwarded_options.api_key.as_deref(), Some("request-key"));
    assert_eq!(forwarded_options.timeout_ms, Some(200));
}

#[tokio::test]
async fn models_cancel_deferred_reports_unsupported_capability_and_unknown_providers() {
    let base = get_builtin_model("openai", "").expect("openai model");
    let handle = DeferredHandle {
        provider: "no-cancel".into(),
        model_id: base.id.clone(),
        api: base.api.clone(),
        id: "h".into(),
        expires_at: None,
        poll_after_ms: None,
        data: None,
    };

    let models = create_models(None);
    let mut model = base.clone();
    model.provider = "no-cancel".into();
    models.set_provider(create_provider(CreateProviderOptions {
        id: "no-cancel".into(),
        name: None,
        base_url: None,
        headers: None,
        models: vec![model.clone()],
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(Arc::new(EchoStreams)),
    }));
    let error = models.cancel_deferred(&model, &handle, None).await.expect_err("unsupported capability");
    assert_eq!(error, "Provider no-cancel does not support deferred responses");

    let mut unknown = base.clone();
    unknown.provider = "nobody".into();
    let error = models.cancel_deferred(&unknown, &handle, None).await.expect_err("unknown provider");
    assert_eq!(error, "Unknown provider: nobody");
}

#[tokio::test]
async fn faux_records_cancellation_and_reports_the_cancelled_fetch_in_band() {
    // Pinned providers.test.ts "records cancellation and returns deferred fetch failures in-band".
    let faux = super::faux::faux_provider(RegisterFauxProviderOptions::default());
    let models = faux_models();
    models.set_provider(faux.provider.clone());
    faux.set_responses(vec![faux_assistant_message("cancelled", FauxAssistantMessageOptions::default()).into()]);
    let model = models.get_models(Some(faux.provider.id())).into_iter().next().expect("faux model");

    let submission = models
        .complete_simple(
            &model,
            &simple_context(),
            Some(SimpleStreamOptions { deferred: Some(DeferredOption::Enabled(true)), ..SimpleStreamOptions::default() }),
            ModelsRequestTransforms::default(),
        )
        .await
        .expect("deferred submission");
    let handle = submission.deferred.clone().expect("deferred handle");

    models.cancel_deferred(&model, &handle, None).await.expect("cancel forwards to faux");
    assert_eq!(faux.state().cancelled_deferred, vec![handle.clone()]);

    let cancelled = models
        .stream_deferred(&model, &handle, None, ModelsRequestTransforms::default())
        .result()
        .await
        .expect("result");
    assert_eq!(cancelled.stop_reason, StopReason::Error);
    assert!(cancelled.error_message.as_deref().is_some_and(|message| message.contains("was cancelled")));
}
