use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{body::Bytes, routing::post, Router};
use maho_ai::api::{anthropic_messages, azure_openai_responses, bedrock_converse_stream,
    google_generative_ai, google_vertex, mistral_conversations, openai_codex_responses,
    openai_completions, openai_responses, pi_messages};
use maho_ai::types::{AssistantMessageEventStream, Context, InputModality, Model, ModelCost,
    ProviderRequestOptions, StreamOptions};
use maho_ai::utils::abort::{AbortController, AbortReason};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Semaphore};

const STREAM_APIS: &[&str] = &[
    "anthropic-messages", "azure-openai-responses", "bedrock-converse-stream",
    "mistral-conversations", "openai-codex-responses", "openai-completions",
    "openai-responses", "pi-messages",
];

struct HookDrop(mpsc::UnboundedSender<()>);

impl Drop for HookDrop {
    fn drop(&mut self) {
        self.0.send(()).expect("hook drop receiver");
    }
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), future).await.expect("bounded event wait")
}

struct WireServer {
    url: String,
    requests: mpsc::UnboundedReceiver<Value>,
    task: tokio::task::JoinHandle<()>,
}

impl WireServer {
    async fn start() -> Self {
        Self::with_body("text/event-stream", "data: {\"choices\":[{\"delta\":{\"content\":\"must not consume\"},\"finish_reason\":\"stop\"}]}\n\n").await
    }

    async fn with_body(content_type: &'static str, response_body: &'static str) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("address"));
        let (tx, requests) = mpsc::unbounded_channel();
        let router = Router::new().fallback(post(move |headers: axum::http::HeaderMap, body: Bytes| {
            let tx = tx.clone();
            async move {
                let body = if headers.get("content-encoding").is_some_and(|value| value == "zstd") {
                    zstd::stream::decode_all(body.as_ref()).expect("zstd wire body")
                } else { body.to_vec() };
                tx.send(serde_json::from_slice(&body).expect("JSON wire payload")).expect("request receiver");
                ([("content-type", content_type)], response_body)
            }
        }));
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.expect("serve");
        });
        Self { url, requests, task }
    }

    async fn close(self) {
        self.task.abort();
        assert!(self.task.await.expect_err("server aborted").is_cancelled());
    }
}

fn model(api: &str, url: &str) -> Model {
    Model {
        id: "test-model".into(), name: "test-model".into(), api: api.into(), provider: "openai".into(),
        base_url: url.into(), reasoning: false, thinking_level_map: None,
        input: vec![InputModality::Text], cost: ModelCost::default(), context_window: 128_000,
        max_tokens: 1024, sampling_params: None, headers: None, cache_retention: None,
        upstream_model_id: None, service_tier: None, recover_text_tool_calls: None, compat: None,
    }
}

fn options(request: ProviderRequestOptions) -> StreamOptions {
    StreamOptions { request: ProviderRequestOptions {
        api_key: Some("loopback-key".into()), max_retries: Some(0),
        env: Some([("AWS_BEDROCK_SKIP_AUTH".into(), "1".into())].into()), ..request
    }, ..Default::default() }
}

fn start(model: &Model, options: StreamOptions) -> AssistantMessageEventStream {
    let context = Context { system_prompt: None, messages: vec![], tools: None };
    match model.api.as_str() {
        "anthropic-messages" => anthropic_messages::stream(model, &context, Some(options)),
        "azure-openai-responses" => azure_openai_responses::stream(model, &context, Some(options)),
        "bedrock-converse-stream" => bedrock_converse_stream::stream(model, &context, Some(options)),
        "google-generative-ai" => google_generative_ai::stream(model, &context, Some(options)),
        "google-vertex" => google_vertex::stream(model, &context, Some(options)),
        "mistral-conversations" => mistral_conversations::stream(model, &context, Some(options)),
        "openai-codex-responses" => openai_codex_responses::stream(model, &context, Some(options)),
        "openai-completions" => openai_completions::stream(model, &context,
            Some(openai_completions::OpenAiCompletionsOptions { stream: options, ..Default::default() })),
        "openai-responses" => openai_responses::stream(model, &context, Some(options)),
        "pi-messages" => pi_messages::stream(model, &context, Some(options)),
        api => panic!("unmapped API {api}"),
    }
}

#[tokio::test]
async fn delayed_payload_replacement_reaches_wire_before_response_error_stops_consumption() {
    for api in STREAM_APIS {
        let mut server = WireServer::start().await;
        let model = model(api, &server.url);
        let (payload_tx, mut payload_rx) = mpsc::unbounded_channel();
        let payload_release = Arc::new(Semaphore::new(0));
        let (response_tx, mut response_rx) = mpsc::unbounded_channel();
        let response_release = Arc::new(Semaphore::new(0));
        let order = Arc::new(Mutex::new(Vec::new()));
        let sync_order = order.clone();
        let async_order = order.clone();
        let response_order = order.clone();
        let response_sync_order = order.clone();
        let request = ProviderRequestOptions {
            on_payload: Some(Arc::new(move |payload, _, _| {
                sync_order.lock().expect("order").push("sync payload");
                let mut payload = payload.clone();
                payload["hook_probe"] = json!("sync");
                Some(payload)
            })),
            async_on_payload: Some(Arc::new({
                let release = payload_release.clone();
                move |mut payload, _, _| {
                    let release = release.clone();
                    let tx = payload_tx.clone();
                    let order = async_order.clone();
                    Box::pin(async move {
                        assert_eq!(payload["hook_probe"], "sync");
                        order.lock().expect("order").push("async payload");
                        tx.send(()).expect("entered payload");
                        release.acquire().await.expect("payload permit").forget();
                        payload["hook_probe"] = json!("async");
                        Ok(Some(payload))
                    })
                }
            })),
            on_response: Some(Arc::new(move |response, _| {
                assert_eq!(response.status, 200);
                response_sync_order.lock().expect("order").push("sync response");
            })),
            async_on_response: Some(Arc::new({
                let release = response_release.clone();
                move |_, _| {
                    let tx = response_tx.clone();
                    let release = release.clone();
                    let order = response_order.clone();
                    Box::pin(async move {
                        order.lock().expect("order").push("async response");
                        tx.send(()).expect("entered response");
                        release.acquire().await.expect("response permit").forget();
                        Err("response-hook-exact-error".into())
                    })
                }
            })),
            ..Default::default()
        };
        let stream = start(&model, options(request));
        assert_eq!(bounded(payload_rx.recv()).await, Some(()), "{api}");
        assert!(matches!(server.requests.try_recv(), Err(mpsc::error::TryRecvError::Empty)), "{api}: transmitted before payload release");
        payload_release.add_permits(1);
        let wire = bounded(server.requests.recv()).await.expect("wire request");
        assert_eq!(wire["hook_probe"], "async", "{api}");
        assert_eq!(bounded(response_rx.recv()).await, Some(()), "{api}");
        assert!(stream.queue().is_empty(), "{api}: consumed before response release");
        response_release.add_permits(1);
        let result = bounded(stream.result()).await.expect("terminal message");
        assert_eq!(result.error_message.as_deref(), Some("response-hook-exact-error"), "{api}");
        assert!(result.content.is_empty(), "{api}: consumed after hook error");
        let events = bounded(stream.collect()).await.expect("events");
        assert!(events.iter().all(|event| matches!(event, maho_ai::types::AssistantMessageEvent::Error { .. })), "{api}: post-hook stream event");
        assert_eq!(*order.lock().expect("order"), ["sync payload", "async payload", "sync response", "async response"], "{api}");
        server.close().await;
    }
}

#[tokio::test]
async fn pending_payload_cancellation_and_errors_prevent_transmission_for_every_payload_api() {
    for api in STREAM_APIS.iter().copied().chain(["google-generative-ai", "google-vertex"]) {
        for cancel in [false, true] {
            let mut server = WireServer::start().await;
            let model = model(api, &server.url);
            let controller = AbortController::new();
            let (tx, mut rx) = mpsc::unbounded_channel();
            let (drop_tx, mut drop_rx) = mpsc::unbounded_channel();
            let release = Arc::new(Semaphore::new(0));
            let request = ProviderRequestOptions {
                signal: Some(controller.signal()),
                async_on_payload: Some(Arc::new({
                    let release = release.clone();
                    move |_, _, _| {
                        let tx = tx.clone();
                        let drop_tx = drop_tx.clone();
                        let release = release.clone();
                        Box::pin(async move {
                            let _drop = HookDrop(drop_tx);
                            tx.send(()).expect("entered");
                            release.acquire().await.expect("permit").forget();
                            Err("payload-hook-exact-error".into())
                        })
                    }
                })), ..Default::default()
            };
            let stream = start(&model, options(request));
            assert_eq!(bounded(rx.recv()).await, Some(()), "{api}");
            if cancel {
                controller.abort(Some(AbortReason::new("AbortError", "cancel-pending-payload")));
            } else {
                release.add_permits(1);
            }
            let result = bounded(stream.result()).await.expect("terminal message");
            assert_eq!(bounded(drop_rx.recv()).await, Some(()), "{api}: payload future retained");
            assert!(result.content.is_empty(), "{api}");
            if !cancel {
                assert_eq!(result.error_message.as_deref(), Some("payload-hook-exact-error"), "{api}");
            }
            assert!(matches!(server.requests.try_recv(), Err(mpsc::error::TryRecvError::Empty)), "{api}: transmitted after failed hook");
            server.close().await;
        }
    }
}

#[tokio::test]
async fn cancellation_drops_pending_response_hook_without_consuming_stream() {
    for api in STREAM_APIS {
        let mut server = WireServer::start().await;
        let model = model(api, &server.url);
        let controller = AbortController::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (drop_tx, mut drop_rx) = mpsc::unbounded_channel();
        let release = Arc::new(Semaphore::new(0));
        let request = ProviderRequestOptions {
            signal: Some(controller.signal()),
            async_on_response: Some(Arc::new(move |_, _| {
                let tx = tx.clone();
                let drop_tx = drop_tx.clone();
                let release = release.clone();
                Box::pin(async move {
                    let _drop = HookDrop(drop_tx);
                    tx.send(()).expect("entered response");
                    release.acquire().await.expect("permit").forget();
                    panic!("cancelled hook must never resume");
                })
            })), ..Default::default()
        };
        let stream = start(&model, options(request));
        bounded(server.requests.recv()).await.expect("request");
        assert_eq!(bounded(rx.recv()).await, Some(()), "{api}");
        assert!(stream.queue().is_empty(), "{api}: events before cancellation");
        controller.abort(Some(AbortReason::new("AbortError", "cancel-pending-response")));
        let result = bounded(stream.result()).await.expect("terminal message");
        assert_eq!(bounded(drop_rx.recv()).await, Some(()), "{api}: response future retained");
        assert!(result.content.is_empty(), "{api}: consumed after cancellation");
        assert!(result.error_message.is_some(), "{api}");
        let events = bounded(stream.collect()).await.expect("events");
        assert!(events.iter().all(|event| matches!(event, maho_ai::types::AssistantMessageEvent::Error { .. })), "{api}");
        server.close().await;
    }
}

fn image_model(api: &str, url: &str) -> maho_ai::types::ImagesModel {
    maho_ai::types::ImagesModel {
        id: "gpt-image-1".into(), name: "image".into(), api: api.into(), provider: "openai".into(),
        base_url: url.into(), thinking_level_map: None, input: vec![InputModality::Text],
        output: vec![maho_ai::types::ImagesOutputModality::Image],
        cost: Default::default(), sampling_params: None, headers: None, cache_retention: None,
        upstream_model_id: None, service_tier: None, recover_text_tool_calls: None,
    }
}

#[tokio::test]
async fn image_hooks_gate_wire_and_parsed_response_output_with_errors_and_cancellation() {
    for api in ["openai-images", "openrouter-images"] {
        for cancel in [false, true] {
            let body = if api == "openai-images" { "{\"data\":[{\"b64_json\":\"AA==\"}]}" }
                else { "{\"choices\":[{\"message\":{\"images\":[{\"image_url\":{\"url\":\"data:image/png;base64,AA==\"}}]}}]}" };
            let mut server = WireServer::with_body("application/json", body).await;
            let model = image_model(api, &server.url);
            let controller = AbortController::new();
            let (payload_tx, mut payload_rx) = mpsc::unbounded_channel();
            let payload_release = Arc::new(Semaphore::new(0));
            let (response_tx, mut response_rx) = mpsc::unbounded_channel();
            let response_release = Arc::new(Semaphore::new(0));
            let request = ProviderRequestOptions {
                api_key: Some("loopback-key".into()), signal: Some(controller.signal()), max_retries: Some(0),
                async_on_payload: Some(Arc::new({
                    let release = payload_release.clone();
                    move |mut payload, _, _| {
                        let release = release.clone();
                        let tx = payload_tx.clone();
                        Box::pin(async move {
                            tx.send(()).expect("payload entered");
                            release.acquire().await.expect("permit").forget();
                            payload["hook_probe"] = json!("image-async");
                            Ok(Some(payload))
                        })
                    }
                })),
                async_on_response: Some(Arc::new({
                    let release = response_release.clone();
                    move |response, _| {
                        assert_eq!(response.status, 200);
                        let tx = response_tx.clone();
                        let release = release.clone();
                        Box::pin(async move {
                            tx.send(()).expect("response entered");
                            release.acquire().await.expect("permit").forget();
                            Err("image-response-exact-error".into())
                        })
                    }
                })), ..Default::default()
            };
            let task = tokio::spawn(async move {
                let context = maho_ai::types::ImagesContext { input: vec![maho_ai::types::ContentBlock::text("draw")] };
                let options = Some(maho_ai::types::ImagesOptions { request, ..Default::default() });
                match api {
                    "openai-images" => maho_ai::api::openai_images::generate_images(&model, &context, options).await,
                    _ => maho_ai::api::openrouter_images::generate_images(&model, &context, options).await,
                }
            });
            assert_eq!(bounded(payload_rx.recv()).await, Some(()));
            assert!(matches!(server.requests.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
            payload_release.add_permits(1);
            assert_eq!(bounded(server.requests.recv()).await.expect("wire")["hook_probe"], "image-async");
            assert_eq!(bounded(response_rx.recv()).await, Some(()));
            assert!(!task.is_finished(), "{api}: constructed output before response release");
            if cancel {
                controller.abort(Some(AbortReason::new("AbortError", "cancel-image-response")));
            } else {
                response_release.add_permits(1);
            }
            let result = bounded(task).await.expect("image task");
            assert!(result.output.is_empty());
            assert_eq!(result.error_message.as_deref(), Some(if cancel { "cancel-image-response" } else { "image-response-exact-error" }), "{api}");
            server.close().await;
        }
    }
}

#[tokio::test]
async fn image_payload_validation_still_rejects_async_non_object_replacements_before_wire() {
    let mut server = WireServer::start().await;
    let model = image_model("openai-images", &server.url);
    let context = maho_ai::types::ImagesContext { input: vec![maho_ai::types::ContentBlock::text("draw")] };
    let result = bounded(maho_ai::api::openai_images::generate_images(&model, &context,
        Some(maho_ai::types::ImagesOptions {
            request: ProviderRequestOptions {
                async_on_payload: Some(Arc::new(|_, _, _| Box::pin(async { Ok(Some(json!("invalid"))) }))),
                ..Default::default()
            }, ..Default::default()
        }))).await;
    assert_eq!(result.error_message.as_deref(), Some("onPayload returned an invalid image generation payload"));
    assert!(matches!(server.requests.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
    server.close().await;
}

#[tokio::test]
async fn warm_cache_awaits_metadata_hook_then_sanitizes_wire_and_ignores_response_hook() {
    let mut server = WireServer::start().await;
    let address: std::net::SocketAddr = server.url.strip_prefix("http://").expect("url").parse().expect("address");
    let model = model("anthropic-messages", &format!("http://api.anthropic.com:{}", address.port()));
    let client = reqwest::Client::builder().no_proxy().resolve("api.anthropic.com", address).build().expect("client");
    let (tx, mut rx) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    let options = StreamOptions { request: ProviderRequestOptions {
        fetch: Some(client), api_key: Some("key".into()),
        async_on_payload: Some(Arc::new({
            let release = release.clone();
            move |mut payload, model, metadata| {
                let tx = tx.clone();
                let release = release.clone();
                Box::pin(async move {
                    assert_eq!(metadata.expect("cache metadata").model.id, model.id);
                    tx.send(()).expect("entered");
                    release.acquire().await.expect("permit").forget();
                    payload["hook_probe"] = json!("cache-async");
                    payload["max_tokens"] = json!(100);
                    payload["thinking"] = json!({"type":"enabled"});
                    Ok(Some(payload))
                })
            }
        })),
        async_on_response: Some(Arc::new(|_, _| Box::pin(async { panic!("cache has no source response hook") }))),
        ..Default::default()
    }, ..Default::default() };
    let task = async move {
        maho_ai::api::warm_prompt_cache::warm_prompt_cache(&model, &Context::default(), Some(options),
            &|_, _, _| Ok(json!({"model":"cache-model", "messages":[], "stream":true}))).await
    };
    let observe = async {
        assert_eq!(bounded(rx.recv()).await, Some(()));
        assert!(matches!(server.requests.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        release.add_permits(1);
        let wire = bounded(server.requests.recv()).await.expect("cache wire");
        assert_eq!(wire["hook_probe"], "cache-async");
        assert_eq!(wire["max_tokens"], 0);
        assert!(wire.get("stream").is_none());
        assert!(wire.get("thinking").is_none());
    };
    let (result, ()) = bounded(async { tokio::join!(task, observe) }).await;
    assert!(result.is_err());
    server.close().await;
}

#[tokio::test]
async fn bai_normalizes_after_both_upstream_hooks_once_on_actual_wire() {
    let mut server = WireServer::start().await;
    let model = model("openai-responses", &server.url);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let sync_count = count.clone();
    let request = ProviderRequestOptions {
        on_payload: Some(Arc::new(move |_, _, _| {
            sync_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            None
        })),
        async_on_payload: Some(Arc::new({
            let release = release.clone();
            move |mut payload, _, _| {
                let tx = tx.clone();
                let release = release.clone();
                Box::pin(async move {
                    tx.send(()).expect("entered");
                    release.acquire().await.expect("permit").forget();
                    payload["tools"] = json!([{"type":"function", "name":"tool", "parameters":{"properties":{"value":{"type":"string"}}}}]);
                    Ok(Some(payload))
                })
            }
        })),
        async_on_response: Some(Arc::new(|_, _| Box::pin(async { Err("bai-response-stop".into()) }))),
        ..Default::default()
    };
    struct Responses;
    impl maho_ai::types::ProviderStreams for Responses {
        fn stream(&self, model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
            openai_responses::stream(model, context, options)
        }
        fn stream_simple(&self, model: &Model, context: &Context, options: Option<maho_ai::types::SimpleStreamOptions>) -> AssistantMessageEventStream {
            openai_responses::stream_simple(model, context, options)
        }
    }
    let streams = maho_ai::providers::bai_stream::bai_responses_streams(Arc::new(Responses));
    let stream = streams.stream(&model, &Context::default(), Some(options(request)));
    assert_eq!(bounded(rx.recv()).await, Some(()));
    assert!(matches!(server.requests.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
    release.add_permits(1);
    let wire = bounded(server.requests.recv()).await.expect("wire");
    assert_eq!(wire["tools"][0]["parameters"]["type"], "object");
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(bounded(stream.result()).await.expect("result").error_message.as_deref(), Some("bai-response-stop"));
    server.close().await;
}

#[tokio::test]
async fn faux_response_hooks_block_factory_and_deferred_fetch_on_error_or_cancellation() {
    use maho_ai::providers::faux::{create_faux_core, faux_assistant_message, FauxResponseStep, RegisterFauxProviderOptions};
    for cancel in [false, true] {
        let core = create_faux_core(&RegisterFauxProviderOptions::default());
        let model = core.get_model(None).expect("model");
        core.set_responses(vec![FauxResponseStep::Factory(Arc::new(|_, _, _, _| Box::pin(async {
            panic!("factory must not run after response hook failure")
        })))]);
        let controller = AbortController::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let release = Arc::new(Semaphore::new(0));
        let request = ProviderRequestOptions {
            signal: Some(controller.signal()),
            async_on_response: Some(Arc::new({
                let release = release.clone();
                move |_, _| {
                    let tx = tx.clone();
                    let release = release.clone();
                    Box::pin(async move {
                        tx.send(()).expect("entered");
                        release.acquire().await.expect("permit").forget();
                        Err("faux-response-error".into())
                    })
                }
            })), ..Default::default()
        };
        let streams = maho_ai::providers::faux::faux_streams(core.clone());
        let stream = streams.stream(&model, &Context::default(), Some(StreamOptions { request: request.clone(), ..Default::default() }));
        assert_eq!(bounded(rx.recv()).await, Some(()));
        assert!(stream.queue().is_empty());
        if cancel { controller.abort(Some(AbortReason::new("AbortError", "faux-cancel"))); }
        else { release.add_permits(1); }
        let result = bounded(stream.result()).await.expect("result");
        assert_eq!(result.error_message.as_deref(), Some(if cancel { "faux-cancel" } else { "faux-response-error" }));
        core.set_responses(vec![faux_assistant_message("deferred", Default::default()).into()]);
        let deferred = streams.stream_simple(&model, &Context::default(), Some(maho_ai::types::SimpleStreamOptions {
            deferred: Some(maho_ai::types::DeferredOption::Enabled(true)), ..Default::default()
        }));
        let handle = bounded(deferred.result()).await.expect("deferred").deferred.expect("handle");
        let fetch = streams.fetch_deferred(&model, &handle, Some(maho_ai::types::DeferredFetchOptions { request, ..Default::default() })).expect("fetch");
        if !cancel {
            assert_eq!(bounded(rx.recv()).await, Some(()));
            assert!(fetch.queue().is_empty());
            release.add_permits(1);
        }
        assert!(bounded(fetch.result()).await.expect("fetch result").error_message.is_some());
    }
}

#[tokio::test]
async fn faux_cancel_marks_handle_before_awaited_response_hook_and_propagates_error() {
    use maho_ai::providers::faux::{create_faux_core, faux_assistant_message, faux_streams, RegisterFauxProviderOptions};
    let core = create_faux_core(&RegisterFauxProviderOptions::default());
    let model = core.get_model(None).expect("model");
    core.set_responses(vec![faux_assistant_message("deferred", Default::default()).into()]);
    let streams = faux_streams(core.clone());
    let deferred = streams.stream_simple(&model, &Context::default(), Some(maho_ai::types::SimpleStreamOptions {
        deferred: Some(maho_ai::types::DeferredOption::Enabled(true)), ..Default::default()
    }));
    let handle = bounded(deferred.result()).await.expect("result").deferred.expect("handle");
    let (tx, mut rx) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    let cancel_options = ProviderRequestOptions {
        async_on_response: Some(Arc::new({
            let release = release.clone();
            move |response, _| {
                assert_eq!(response.status, 200);
                let tx = tx.clone();
                let release = release.clone();
                Box::pin(async move {
                    tx.send(()).expect("entered");
                    release.acquire().await.expect("permit").forget();
                    Err("faux-cancel-hook-error".into())
                })
            }
        })), ..Default::default()
    };
    let task = tokio::spawn({
        let core = core.clone();
        let model = model.clone();
        let handle = handle.clone();
        async move { core.cancel_deferred_with_options(&model, &handle, Some(cancel_options)).await }
    });
    assert_eq!(bounded(rx.recv()).await, Some(()));
    assert_eq!(core.state().cancelled_deferred.as_slice(), std::slice::from_ref(&handle));
    assert!(!task.is_finished());
    release.add_permits(1);
    assert_eq!(bounded(task).await.expect("task"), Err("faux-cancel-hook-error".into()));
    let fetch = streams.fetch_deferred(&model, &handle, None).expect("fetch");
    assert!(bounded(fetch.result()).await.expect("result").error_message.expect("error").contains("cancelled"));
}

#[test]
fn common_simple_option_builder_forwards_async_callbacks_without_replacement() {
    let payload: maho_ai::types::AsyncOnPayload = Arc::new(|_, _, _| Box::pin(async { Ok(None) }));
    let response: maho_ai::types::AsyncOnResponse = Arc::new(|_, _| Box::pin(async { Ok(()) }));
    let model = model("openai-completions", "http://127.0.0.1:1");
    let simple = maho_ai::types::SimpleStreamOptions {
        stream: StreamOptions { request: ProviderRequestOptions {
            async_on_payload: Some(payload.clone()), async_on_response: Some(response.clone()),
            ..Default::default()
        }, ..Default::default() }, ..Default::default()
    };
    let context = Context::default();
    let common = maho_ai::api::simple_options::build_base_options(&model, &context, Some(&simple), None).expect("common");
    assert!(Arc::ptr_eq(common.request.async_on_payload.as_ref().expect("payload"), &payload));
    assert!(Arc::ptr_eq(common.request.async_on_response.as_ref().expect("response"), &response));
}

#[tokio::test]
async fn completions_simple_builder_forwards_both_async_hooks_to_wire() {
    let mut server = WireServer::start().await;
    let model = model("openai-completions", &server.url);
    let request = ProviderRequestOptions {
        async_on_payload: Some(Arc::new(|mut payload, _, _| Box::pin(async move {
            payload["hook_probe"] = json!("simple-async");
            Ok(Some(payload))
        }))),
        async_on_response: Some(Arc::new(|_, _| Box::pin(async { Err("simple-response-error".into()) }))),
        ..Default::default()
    };
    let stream = openai_completions::stream_simple(&model, &Context::default(), Some(maho_ai::types::SimpleStreamOptions {
        stream: options(request), ..Default::default()
    }));
    assert_eq!(bounded(server.requests.recv()).await.expect("wire")["hook_probe"], "simple-async");
    assert_eq!(bounded(stream.result()).await.expect("result").error_message.as_deref(), Some("simple-response-error"));
    server.close().await;
}

#[tokio::test]
async fn google_and_vertex_async_replacements_reach_projected_wire_without_response_callbacks() {
    for api in ["google-generative-ai", "google-vertex"] {
        let mut server = WireServer::start().await;
        let model = model(api, &server.url);
        let request = ProviderRequestOptions {
            async_on_payload: Some(Arc::new(|mut payload, _, _| Box::pin(async move {
                payload["config"]["temperature"] = json!(0.123);
                Ok(Some(payload))
            }))),
            async_on_response: Some(Arc::new(|_, _| Box::pin(async { panic!("Google source has no response hook") }))),
            ..Default::default()
        };
        let stream = start(&model, options(request));
        let wire = bounded(server.requests.recv()).await.expect("wire");
        assert_eq!(wire["generationConfig"]["temperature"], 0.123, "{api}");
        bounded(stream.result()).await.expect("result");
        server.close().await;
    }
}

#[tokio::test]
async fn anthropic_exhausted_http_failure_awaits_numeric_status_hook_once() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let model = model("anthropic-messages", &format!("http://{}", listener.local_addr().expect("address")));
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().fallback(post(|| async {
            (axum::http::StatusCode::BAD_REQUEST, "{\"error\":{\"message\":\"provider rejected\"}}")
        }))).await.expect("serve");
    });
    let (tx, mut rx) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    let stream = start(&model, options(ProviderRequestOptions {
        async_on_response: Some(Arc::new({
            let release = release.clone();
            move |response, _| {
                assert_eq!(response.status, 400);
                assert!(response.headers.is_empty());
                let tx = tx.clone();
                let release = release.clone();
                Box::pin(async move {
                    tx.send(()).expect("entered");
                    release.acquire().await.expect("permit").forget();
                    Err("anthropic-status-hook-error".into())
                })
            }
        })), ..Default::default()
    }));
    assert_eq!(bounded(rx.recv()).await, Some(()));
    assert!(stream.queue().is_empty());
    release.add_permits(1);
    assert_eq!(bounded(stream.result()).await.expect("result").error_message.as_deref(), Some("anthropic-status-hook-error"));
    assert!(matches!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected)));
    server.abort();
    assert!(server.await.expect_err("aborted").is_cancelled());
}

#[tokio::test]
async fn rejected_image_http_responses_do_not_invoke_source_success_only_hook() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("address"));
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().fallback(post(|| async {
            (axum::http::StatusCode::BAD_REQUEST, "{\"error\":{\"message\":\"image rejected\"}}")
        }))).await.expect("serve");
    });
    for api in ["openai-images", "openrouter-images"] {
        let model = image_model(api, &url);
        let options = Some(maho_ai::types::ImagesOptions {
            request: ProviderRequestOptions {
                api_key: Some("key".into()), max_retries: Some(0),
                on_response: Some(Arc::new(|_, _| panic!("legacy response callback is success only"))),
                async_on_response: Some(Arc::new(|_, _| Box::pin(async { panic!("async response callback is success only") }))),
                ..Default::default()
            }, ..Default::default()
        });
        let context = maho_ai::types::ImagesContext { input: vec![maho_ai::types::ContentBlock::text("draw")] };
        let result = match api {
            "openai-images" => bounded(maho_ai::api::openai_images::generate_images(&model, &context, options)).await,
            _ => bounded(maho_ai::api::openrouter_images::generate_images(&model, &context, options)).await,
        };
        assert!(result.error_message.expect("error").contains("image rejected"));
    }
    server.abort();
    assert!(server.await.expect_err("aborted").is_cancelled());
}
