//! Shared harness for the ported `packages/ai/test/openai-completions-*.test.ts` cases.
//!
//! Two fakes stand in for what the TS tests inject:
//! `ScriptedTransport` replaces `vi.mock("openai")` (a scripted SDK client that records the
//! request bodies/options it was called with), and `raw_server` replaces the `node:http`
//! `startServer` helper (raw bytes written to a socket, so a case can truncate a stream or stall
//! the response headers).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use maho_ai::api::openai_completions::{
    stream_simple_with_transport, stream_with_transport, ChatRequest, ChatStreamResponse, ChatTransport,
    OpenAiCompletionsError, OpenAiCompletionsOptions,
};
use futures::StreamExt;
use maho_ai::types::{
    ContentBlock, Context, Message, Model, SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingContent,
    Tool, ToolCall, ToolResultMessage, UserContent,
};
use serde_json::{json, Value};

#[derive(Debug, Clone, Default)]
pub struct CapturedRequest {
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

impl CapturedRequest {
    pub fn messages(&self) -> &[Value] {
        self.body.get("messages").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default()
    }

    pub fn tools(&self) -> &[Value] {
        self.body.get("tools").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default()
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.body.get(key)
    }
}

#[derive(Default)]
pub struct ScriptedResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub chunks: Vec<Result<Value, OpenAiCompletionsError>>,
}

impl ScriptedResponse {
    pub fn chunks(chunks: impl IntoIterator<Item = Value>) -> Self {
        Self { status: 200, headers: BTreeMap::new(), chunks: chunks.into_iter().map(Ok).collect() }
    }

    pub fn failing(error: OpenAiCompletionsError) -> Self {
        Self { status: 200, headers: BTreeMap::new(), chunks: vec![Err(error)] }
    }

    pub fn chunks_then_error(chunks: impl IntoIterator<Item = Value>, error: OpenAiCompletionsError) -> Self {
        let mut items: Vec<Result<Value, OpenAiCompletionsError>> = chunks.into_iter().map(Ok).collect();
        items.push(Err(error));
        Self { status: 200, headers: BTreeMap::new(), chunks: items }
    }
}

#[derive(Default)]
pub struct ScriptedTransport {
    responses: Mutex<std::collections::VecDeque<Result<ScriptedResponse, OpenAiCompletionsError>>>,
    requests: Mutex<Vec<CapturedRequest>>,
}

impl ScriptedTransport {
    pub fn new(responses: impl IntoIterator<Item = Result<ScriptedResponse, OpenAiCompletionsError>>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        })
    }

    pub fn success(chunks: impl IntoIterator<Item = Value>) -> Arc<Self> {
        Self::new([Ok(ScriptedResponse::chunks(chunks))])
    }

    pub fn requests(&self) -> Vec<CapturedRequest> {
        self.requests.lock().expect("requests lock").clone()
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().expect("requests lock").len()
    }

    pub fn last_request(&self) -> CapturedRequest {
        self.requests.lock().expect("requests lock").last().cloned().expect("a captured request")
    }
}

impl ChatTransport for ScriptedTransport {
    fn create_chat_completion(
        &self,
        request: ChatRequest,
    ) -> maho_ai::types::BoxFuture<'static, Result<ChatStreamResponse, OpenAiCompletionsError>> {
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let headers = request
            .headers
            .iter()
            .filter_map(|(name, value)| value.to_str().ok().map(|value| (name.as_str().to_lowercase(), value.to_owned())))
            .collect();
        self.requests.lock().expect("requests lock").push(CapturedRequest { headers, body });
        let response = self.responses.lock().expect("responses lock").pop_front();
        Box::pin(async move {
            match response {
                Some(Ok(response)) => Ok(ChatStreamResponse {
                    stream: futures::stream::iter(response.chunks).boxed(),
                    status: response.status,
                    headers: response.headers,
                }),
                Some(Err(error)) => Err(error),
                None => Err(OpenAiCompletionsError::protocol("no scripted response")),
            }
        })
    }
}

pub fn chunk(delta: Value, finish_reason: Option<&str>) -> Value {
    json!({
        "id": "chatcmpl-test",
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish_reason.map_or(Value::Null, |reason| Value::String(reason.to_owned())),
        }],
    })
}


pub fn catalog_model(provider: &str, id: &str) -> Model {
    maho_ai::compat::get_model(provider, id)
        .unwrap_or_else(|error| panic!("catalog model {provider}/{id}: {error}"))
        .clone()
}

pub fn model(overrides: &[(&str, Value)]) -> Model {
    let mut value = json!({
        "id": "test-model",
        "name": "Test Model",
        "api": "openai-completions",
        "provider": "openai",
        "baseUrl": "https://api.openai.com/v1",
        "reasoning": false,
        "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 128000,
        "maxTokens": 4096,
    });
    for (key, override_value) in overrides {
        value[key] = override_value.clone();
    }
    serde_json::from_value(value).expect("model")
}

pub fn context(messages: Vec<Message>, tools: Option<Vec<Tool>>) -> Context {
    Context { system_prompt: None, messages, tools }
}

pub fn user_message(text: &str) -> Message {
    Message::User(maho_ai::types::UserMessage {
        content: UserContent::Text(text.to_owned()),
        timestamp: 0,
    })
}

pub fn user_blocks(blocks: Vec<ContentBlock>) -> Message {
    Message::User(maho_ai::types::UserMessage { content: UserContent::Blocks(blocks), timestamp: 0 })
}

pub fn text_block(text: &str) -> ContentBlock {
    ContentBlock::Text(TextContent { text: text.to_owned(), ..TextContent::default() })
}

pub fn thinking_block(thinking: &str, signature: Option<&str>) -> ContentBlock {
    ContentBlock::Thinking(ThinkingContent {
        thinking: thinking.to_owned(),
        thinking_signature: signature.map(str::to_owned),
        ..ThinkingContent::default()
    })
}






pub fn tool(name: &str, parameters: Value) -> Tool {
    Tool {
        name: name.to_owned(),
        description: format!("{name} tool"),
        parameters,
        freeform: None,
        constrained_sampling: None,
    }
}

pub fn tool_call(id: &str, name: &str, arguments: Value) -> ContentBlock {
    ContentBlock::ToolCall(ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments: arguments.as_object().cloned().unwrap_or_default(),
        incomplete: None,
        error_message: None,
        thought_signature: None,
        namespace: None,
    })
}

pub fn tool_result(tool_call_id: &str, content: Vec<ContentBlock>) -> Message {
    Message::ToolResult(ToolResultMessage {
        tool_call_id: tool_call_id.to_owned(),
        tool_name: "read".to_owned(),
        content,
        details: None,
        usage: None,
        added_tool_names: None,
        is_error: false,
        timestamp: 0,
    })
}

pub fn assistant(content: Vec<ContentBlock>, stop_reason: StopReason) -> Message {
    Message::Assistant(Box::new(maho_ai::types::AssistantMessage {
        content,
        api: "openai-completions".to_owned(),
        provider: "openrouter".to_owned(),
        model: "test-model".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        diagnostics: None,
        usage: Default::default(),
        stop_reason,
        stop_details: None,
        deferred: None,
        error_message: None,
        abort_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
    }))
}

pub fn options(api_key: &str) -> OpenAiCompletionsOptions {
    OpenAiCompletionsOptions {
        stream: StreamOptions { request: maho_ai::types::ProviderRequestOptions { api_key: Some(api_key.to_owned()), ..Default::default() }, ..Default::default() },
        ..Default::default()
    }
}

pub fn stream_options(api_key: &str) -> StreamOptions {
    StreamOptions {
        request: maho_ai::types::ProviderRequestOptions { api_key: Some(api_key.to_owned()), ..Default::default() },
        ..Default::default()
    }
}

pub fn simple_options(api_key: &str) -> SimpleStreamOptions {
    SimpleStreamOptions { stream: stream_options(api_key), ..Default::default() }
}

pub fn run(model: &Model, context: &Context, options: OpenAiCompletionsOptions, transport: Arc<ScriptedTransport>) -> maho_ai::types::AssistantMessageEventStream {
    stream_with_transport(model, context, Some(options), transport)
}

pub fn run_simple(
    model: &Model,
    context: &Context,
    options: SimpleStreamOptions,
    transport: Option<Arc<dyn ChatTransport>>,
) -> maho_ai::types::AssistantMessageEventStream {
    stream_simple_with_transport(model, context, Some(options), transport)
}

pub async fn finish(stream: &maho_ai::types::AssistantMessageEventStream) -> maho_ai::types::AssistantMessage {
    stream.result().await.expect("stream result")
}

pub async fn events(stream: &maho_ai::types::AssistantMessageEventStream) -> Vec<maho_ai::types::AssistantMessageEvent> {
    stream.collect().await.expect("stream events")
}


type RawHandler = Arc<dyn Fn(tokio::net::TcpStream, Vec<u8>) -> maho_ai::types::BoxFuture<'static, ()> + Send + Sync>;

pub struct RawServer {
    pub base_url: String,
    handle: tokio::task::JoinHandle<()>,
}

impl RawServer {
    pub async fn start(handler: RawHandler) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind raw server");
        let address = listener.local_addr().expect("raw server addr");
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else { return };
                let handler = handler.clone();
                tokio::spawn(async move {
                    let request = read_http_request(&mut stream).await;
                    handler(stream, request).await;
                });
            }
        });
        Self { base_url: format!("http://{address}/v1"), handle }
    }

    pub fn shutdown(self) {
        self.handle.abort();
    }
}

pub fn raw_handler<F, Fut>(handler: F) -> RawHandler
where
    F: Fn(tokio::net::TcpStream, Vec<u8>) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    Arc::new(move |stream, request| Box::pin(handler(stream, request)))
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn content_length(headers: &[u8]) -> usize {
    let text = String::from_utf8_lossy(headers).to_lowercase();
    for line in text.split("\r\n") {
        if let Some(value) = line.strip_prefix("content-length:") {
            return value.trim().parse().unwrap_or(0);
        }
    }
    0
}

async fn read_http_request(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut body_start = None;
    let mut length = 0usize;
    while let Ok(read) = stream.read(&mut chunk).await {
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if body_start.is_none()
            && let Some(position) = find_header_end(&buffer)
        {
            length = content_length(&buffer[..position]);
            body_start = Some(position + 4);
        }
        if let Some(start) = body_start
            && buffer.len() >= start + length
        {
            break;
        }
    }
    buffer
}

pub fn request_body(request: &[u8]) -> Value {
    let start = find_header_end(request).map(|position| position + 4).unwrap_or(0);
    serde_json::from_slice(&request[start..]).unwrap_or(Value::Null)
}

pub async fn write_sse(stream: &mut tokio::net::TcpStream, frames: &[Value], close_after: Option<usize>) {
    use tokio::io::AsyncWriteExt;
    let body: String = frames
        .iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect::<String>()
        + "data: [DONE]\n\n";
    let head = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let bytes = body.as_bytes();
    let cut = close_after.unwrap_or(bytes.len()).min(bytes.len());
    let _ = stream.write_all(&bytes[..cut]).await;
    let _ = stream.flush().await;
    if close_after.is_some() {
        let _ = stream.shutdown().await;
    }
}

