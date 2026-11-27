//! Local Responses HTTP/SSE fixture with a request counter.

use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use maho_ai::api::openai_responses;
use maho_ai::types::{
    AssistantMessage, Context, ModelCompat, ProviderRequestOptions, StreamOptions,
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

pub(super) fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect()
}

pub(super) async fn run(
    status: u16,
    body: String,
    stream: bool,
    siwc: bool,
    max_retries: u32,
) -> (AssistantMessage, usize) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let addr = listener.local_addr().expect("loopback address");
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/responses",
        post({
            let calls = calls.clone();
            move || {
                let body = body.clone();
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let mut headers = HeaderMap::new();
                    headers.insert(
                        "content-type",
                        if stream {
                            "text/event-stream"
                        } else {
                            "application/json"
                        }
                        .parse()
                        .expect("content type"),
                    );
                    if max_retries > 0 {
                        headers.insert("x-should-retry", "true".parse().expect("retry header"));
                        headers.insert("retry-after", "0".parse().expect("retry delay"));
                    }
                    (StatusCode::from_u16(status).expect("status"), headers, body).into_response()
                }
            }
        }),
    );
    let server =
        tokio::spawn(async move { axum::serve(listener, app).await.expect("serve fixture") });
    let mut model = maho_ai::providers::chatgpt_subscription_models::chatgpt_subscription_models()
        .into_iter()
        .next()
        .expect("subscription model");
    model.provider = "openai".into();
    model.api = "openai-responses".into();
    model.base_url = format!("http://{addr}");
    model.compat = Some(ModelCompat(serde_json::Map::from_iter([(
        "siwc".into(),
        json!(siwc),
    )])));
    let context: Context = serde_json::from_value(json!({"messages": []})).expect("context");
    let options = StreamOptions {
        request: ProviderRequestOptions {
            api_key: Some("synthetic-test-token".into()),
            fetch: Some(
                reqwest::Client::builder()
                    .no_proxy()
                    .build()
                    .expect("fixture client"),
            ),
            max_retries: Some(max_retries),
            timeout_ms: Some(5_000),
            ..Default::default()
        },
        ..Default::default()
    };
    let output = openai_responses::stream(&model, &context, Some(options));
    let result = tokio::time::timeout(Duration::from_secs(8), output.result()).await;
    server.abort();
    (
        result.expect("wire deadline").expect("terminal message"),
        calls.load(Ordering::SeqCst),
    )
}
