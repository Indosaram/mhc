//! Port of senpi packages/ai/src/providers/bai-stream.ts.

use crate::types::{
    AssistantMessageEventStream, Context, Model, ProviderRequestMetadata, ProviderStreams, SimpleStreamOptions,
    StreamOptions,
};
use crate::utils::tool_schema_compat::normalize_tool_parameters_for_openai_compat;
use serde_json::Value;
use std::sync::Arc;

fn normalize_responses_function_tool(tool: &Value) -> Value {
    let Some(tool_object) = tool.as_object() else { return tool.clone() };
    if tool_object.get("type").and_then(Value::as_str) != Some("function") {
        return tool.clone();
    }
    let Some(parameters) = tool_object.get("parameters").and_then(Value::as_object) else {
        return tool.clone();
    };
    if parameters.contains_key("type") {
        return tool.clone();
    }
    let mut normalized = tool_object.clone();
    normalized.insert("parameters".to_owned(), Value::Object(normalize_tool_parameters_for_openai_compat(parameters)));
    Value::Object(normalized)
}

pub fn normalize_bai_responses_payload(payload: Value) -> Value {
    let Some(payload_object) = payload.as_object() else { return payload };
    let Some(tools) = payload_object.get("tools").and_then(Value::as_array) else { return payload };
    let transformed: Vec<Value> = tools.iter().map(normalize_responses_function_tool).collect();
    if transformed.iter().zip(tools).all(|(transformed, original)| transformed == original) {
        return payload;
    }
    let mut normalized = payload_object.clone();
    normalized.insert("tools".to_owned(), Value::Array(transformed));
    Value::Object(normalized)
}

fn with_bai_responses_payload(options: StreamOptions) -> StreamOptions {
    let upstream = options.request.clone();
    let hook: crate::types::AsyncOnPayload = Arc::new(
        move |payload: Value, model: Model, request: Option<ProviderRequestMetadata>| {
            let upstream = upstream.clone();
            Box::pin(async move {
                let transformed = upstream.apply_payload_hook(&payload, &model, request.as_ref()).await?;
                Ok(Some(normalize_bai_responses_payload(transformed.unwrap_or(payload))))
            })
        },
    );
    StreamOptions { request: crate::types::ProviderRequestOptions {
        on_payload: None, async_on_payload: Some(hook), ..options.request
    }, ..options }
}

struct BaiResponsesStreams {
    inner: Arc<dyn ProviderStreams>,
}

impl ProviderStreams for BaiResponsesStreams {
    fn stream(&self, model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
        self.inner.stream(model, context, Some(with_bai_responses_payload(options.unwrap_or_default())))
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        let options = options.unwrap_or_default();
        let stream = with_bai_responses_payload(options.stream);
        self.inner.stream_simple(model, context, Some(SimpleStreamOptions { stream, ..options }))
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &crate::types::DeferredHandle,
        options: Option<crate::types::DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        self.inner.fetch_deferred(model, handle, options)
    }

    fn supports_deferred(&self) -> bool {
        self.inner.supports_deferred()
    }
}

pub fn bai_responses_streams(streams: Arc<dyn ProviderStreams>) -> Arc<dyn ProviderStreams> {
    Arc::new(BaiResponsesStreams { inner: streams })
}
