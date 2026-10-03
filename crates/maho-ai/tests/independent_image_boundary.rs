use std::{sync::{Arc, Mutex}, time::Duration};
use axum::{body::Bytes, routing::post, Router};
use maho_ai::{api::{openai_completions, openai_images, openrouter_images}, types::*};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Semaphore};

async fn event<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), f).await.expect("event deadline")
}

fn model(url: &str) -> Model {
    Model { id: "probe".into(), name: "probe".into(), api: "openai-completions".into(),
        provider: "openai".into(), base_url: url.into(), reasoning: false,
        thinking_level_map: None, input: vec![InputModality::Text], cost: ModelCost::default(),
        context_window: 8192, max_tokens: 64, sampling_params: None, headers: None,
        cache_retention: None, upstream_model_id: None, service_tier: None,
        recover_text_tool_calls: None, compat: None }
}

fn image_model(api: &str, url: &str) -> ImagesModel {
    ImagesModel { id: "gpt-image-1".into(), name: "probe".into(), api: api.into(),
        provider: "openai".into(), base_url: url.into(), thinking_level_map: None,
        input: vec![InputModality::Text], output: vec![ImagesOutputModality::Image],
        cost: Default::default(), sampling_params: None, headers: None,
        cache_retention: None, upstream_model_id: None, service_tier: None,
        recover_text_tool_calls: None }
}

struct ServerGuard(tokio::task::JoinHandle<()>);

impl Drop for ServerGuard {
    fn drop(&mut self) { self.0.abort(); }
}

async fn server(body: &'static str, status: axum::http::StatusCode)
    -> (String, mpsc::Receiver<Value>, ServerGuard) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("address"));
    let (tx, rx) = mpsc::channel(1);
    let router = Router::new().fallback(post(move |bytes: Bytes| {
        let tx = tx.clone();
        async move {
            tx.send(serde_json::from_slice(&bytes).expect("wire JSON")).await.expect("wire event");
            (status, [("content-type", "application/json"), ("x-probe", "actual-http")], body)
        }
    }));
    let task = tokio::spawn(async move { axum::serve(listener, router).await.expect("server"); });
    (url, rx, ServerGuard(task))
}

async fn stop(mut task: ServerGuard) {
    task.0.abort();
    assert!(event(&mut task.0).await.expect_err("aborted server").is_cancelled());
}

#[tokio::test]
async fn synchronous_replacement_survives_async_none_and_response_success_on_real_wire() {
    // Given a complete SSE response and independently gated callbacks.
    let (url, mut wire, task) = server("data: {\"id\":\"probe\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"verified\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n", axum::http::StatusCode::OK).await;
    let order = Arc::new(Mutex::new(Vec::new()));
    let (entered, mut entry) = mpsc::channel(1);
    let release = Arc::new(Semaphore::new(0));
    let sync_order = order.clone();
    let payload_order = order.clone();
    let sync_response_order = order.clone();
    let async_response_order = order.clone();
    let callback_release = release.clone();
    let options = StreamOptions { request: ProviderRequestOptions {
        api_key: Some("loopback-only".into()), max_retries: Some(0),
        on_payload: Some(Arc::new(move |payload, _, _| {
            sync_order.lock().expect("order").push("sync-payload");
            let mut next = payload.clone(); next["user"] = json!("sync-survives"); Some(next)
        })),
        async_on_payload: Some(Arc::new(move |payload, _, _| {
            let order = payload_order.clone();
            Box::pin(async move { assert_eq!(payload["user"], "sync-survives");
                order.lock().expect("order").push("async-payload"); Ok(None) })
        })),
        on_response: Some(Arc::new(move |response, _| {
            assert_eq!(response.headers.get("x-probe").map(String::as_str), Some("actual-http"));
            sync_response_order.lock().expect("order").push("sync-response");
        })),
        async_on_response: Some(Arc::new(move |_, _| {
            let entered = entered.clone(); let release = callback_release.clone(); let order = async_response_order.clone();
            Box::pin(async move { order.lock().expect("order").push("async-response");
                entered.send(()).await.expect("entry"); release.acquire().await.expect("release").forget(); Ok(()) })
        })), ..Default::default()
    }, ..Default::default() };
    // When the actual provider receives its HTTP response.
    let stream = openai_completions::stream(&model(&url), &Context::default(),
        Some(openai_completions::OpenAiCompletionsOptions { stream: options, ..Default::default() }));
    assert_eq!(event(wire.recv()).await.expect("request")["user"], "sync-survives");
    event(entry.recv()).await.expect("callback entry");
    assert!(stream.queue().is_empty());
    // Then consumption follows callback settlement and legacy order is preserved.
    release.add_permits(1);
    let result = event(stream.result()).await.expect("result");
    assert_eq!(result.stop_reason, StopReason::Stop, "{result:?}");
    assert!(result.content.iter().any(|item| matches!(item, ContentBlock::Text(text) if text.text == "verified")));
    assert_eq!(order.lock().expect("order").as_slice(),
        ["sync-payload", "async-payload", "sync-response", "async-response"]);
    stop(task).await;
}

struct Dropped(mpsc::Sender<()>);
impl Drop for Dropped {
    fn drop(&mut self) { self.0.try_send(()).expect("drop event"); }
}

#[tokio::test]
async fn dropping_outer_pending_helper_futures_drops_callback_futures() {
    for payload in [false, true] {
        // Given a callback that has entered and cannot complete on its own.
        let (tx, mut rx) = mpsc::channel(1);
        let (drop_tx, mut drop_rx) = mpsc::channel(1);
        let payload_tx = tx.clone(); let payload_drop = drop_tx.clone();
        let request = ProviderRequestOptions {
            async_on_payload: Some(Arc::new(move |_, _, _| {
                let tx = payload_tx.clone(); let drop_tx = payload_drop.clone();
                Box::pin(async move { let _guard = Dropped(drop_tx); tx.send(()).await.expect("entered");
                    std::future::pending::<Result<Option<Value>, String>>().await })
            })),
            async_on_response: Some(Arc::new(move |_, _| {
                let tx = tx.clone(); let drop_tx = drop_tx.clone();
                Box::pin(async move { let _guard = Dropped(drop_tx); tx.send(()).await.expect("entered");
                    std::future::pending::<Result<(), String>>().await })
            })), ..Default::default()
        };
        let model = model("http://127.0.0.1:1");
        let value = json!({}); let response = ProviderResponse { status: 200, headers: Default::default() };
        let mut future: std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> = if payload {
            Box::pin(async { request.apply_payload_hook(&value, &model, None).await.expect("hook"); })
        } else { Box::pin(async { request.apply_response_hook(&response, &model).await.expect("hook"); }) };
        tokio::select! { () = &mut future => panic!("pending hook completed"), entered = event(rx.recv()) => { entered.expect("entry"); } }
        // When the outer future is explicitly dropped, not merely its JoinHandle.
        drop(future);
        // Then the callback Drop event arrives without cancellation or sleeps.
        event(drop_rx.recv()).await.expect("dropped callback");
    }
}

#[tokio::test]
async fn successful_image_http_preserves_async_none_and_awaits_response_before_output() {
    for api in ["openai-images", "openrouter-images"] {
        // Given actual valid image JSON over HTTP with both legacy and async callbacks.
        let body = if api == "openai-images" { "{\"data\":[{\"b64_json\":\"AA==\"}]}" }
            else { "{\"choices\":[{\"message\":{\"images\":[{\"image_url\":{\"url\":\"data:image/png;base64,AA==\"}}]}}]}" };
        let (url, mut wire, server) = server(body, axum::http::StatusCode::OK).await;
        let (tx, mut rx) = mpsc::channel(1); let release = Arc::new(Semaphore::new(0));
        let callback_release = release.clone();
        let request = ProviderRequestOptions { api_key: Some("loopback-only".into()), max_retries: Some(0),
            on_payload: Some(Arc::new(|payload, _, _| { let mut next = payload.clone(); next["probe"] = json!("legacy"); Some(next) })),
            async_on_payload: Some(Arc::new(|payload, _, _| Box::pin(async move { assert_eq!(payload["probe"], "legacy"); Ok(None) }))),
            async_on_response: Some(Arc::new(move |response, _| {
                assert_eq!(response.status, 200); assert_eq!(response.headers["x-probe"], "actual-http");
                let tx = tx.clone(); let release = callback_release.clone();
                Box::pin(async move { tx.send(()).await.expect("entry"); release.acquire().await.expect("release").forget(); Ok(()) })
            })), ..Default::default() };
        // When generation is gated inside the response callback.
        let run = tokio::spawn(async move {
            let model = image_model(api, &url); let context = ImagesContext { input: vec![ContentBlock::text("draw")] };
            let options = Some(ImagesOptions { request, ..Default::default() });
            if api == "openai-images" { openai_images::generate_images(&model, &context, options).await }
            else { openrouter_images::generate_images(&model, &context, options).await }
        });
        assert_eq!(event(wire.recv()).await.expect("wire")["probe"], "legacy");
        event(rx.recv()).await.expect("response entry"); assert!(!run.is_finished()); release.add_permits(1);
        // Then valid output appears only after callback success.
        let result = event(run).await.expect("generation"); assert!(result.error_message.is_none(), "{result:?}");
        assert!(result.output.iter().any(|item| matches!(item, ContentBlock::Image(image) if image.data == "AA==")));
        stop(server).await;
    }
}

#[tokio::test]
async fn malformed_success_image_json_does_not_reach_source_post_parse_callback() {
    let mut mismatches = Vec::new();
    for api in ["openai-images", "openrouter-images"] {
        let (url, mut wire, server) = server("{invalid-json", axum::http::StatusCode::OK).await;
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        let options = Some(ImagesOptions { request: ProviderRequestOptions {
            api_key: Some("loopback-only".into()), max_retries: Some(0),
            async_on_response: Some(Arc::new(move |_, _| {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            })), ..Default::default()
        }, ..Default::default() });
        let model = image_model(api, &url);
        let context = ImagesContext { input: vec![ContentBlock::text("draw")] };
        let result = if api == "openai-images" {
            event(openai_images::generate_images(&model, &context, options)).await
        } else { event(openrouter_images::generate_images(&model, &context, options)).await };
        event(wire.recv()).await.expect("actual request");
        stop(server).await;
        assert!(result.error_message.is_some());
        let count = calls.load(std::sync::atomic::Ordering::SeqCst);
        if count != 0 { mismatches.push((api, count)); }
    }
    assert!(mismatches.is_empty(), "pinned SDK parses before onResponse; unexpected callbacks: {mismatches:?}");
}

#[tokio::test]
async fn image_response_callback_error_never_retries_successful_request() {
    for api in ["openai-images", "openrouter-images"] {
        let body = if api == "openai-images" { "{\"data\":[{\"b64_json\":\"AA==\"}]}" }
            else { "{\"choices\":[{\"message\":{\"images\":[{\"image_url\":{\"url\":\"data:image/png;base64,AA==\"}}]}}]}" };
        let (url, mut wire, server) = server(body, axum::http::StatusCode::OK).await;
        let (tx, mut rx) = mpsc::channel(1);
        let release = Arc::new(Semaphore::new(0));
        let callback_release = release.clone();
        let request = ProviderRequestOptions {
            api_key: Some("loopback-only".into()), max_retries: Some(3),
            async_on_response: Some(Arc::new(move |_, _| {
                let tx = tx.clone();
                let release = callback_release.clone();
                Box::pin(async move {
                    tx.send(()).await.expect("callback entry");
                    release.acquire().await.expect("release").forget();
                    Err("429 image-callback-exact-error".into())
                })
            })), ..Default::default()
        };
        let run = tokio::spawn(async move {
            let model = image_model(api, &url);
            let context = ImagesContext { input: vec![ContentBlock::text("draw")] };
            let options = Some(ImagesOptions { request, ..Default::default() });
            if api == "openai-images" { openai_images::generate_images(&model, &context, options).await }
            else { openrouter_images::generate_images(&model, &context, options).await }
        });
        event(wire.recv()).await.expect("first request");
        event(rx.recv()).await.expect("callback entered");
        assert!(!run.is_finished());
        release.add_permits(1);
        let result = event(run).await.expect("generation");
        assert_eq!(result.error_message.as_deref(), Some("429 image-callback-exact-error"));
        assert!(matches!(wire.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        assert!(matches!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected)));
        stop(server).await;
    }
}
