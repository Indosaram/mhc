//! Port of the wire-protocol cases in senpi `packages/ai/test/cursor-agent.test.ts`
//! ("cursor-agent wire protocol") and the exec-channel cases those exercise.
//!
//! Each case stands up a real HTTP/2 server, drives `cursor_agent::stream` against
//! it, and asserts on the events, the assembled assistant message, and the frames
//! the client sent back. Ported by todo 12.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use h2::server;
use prost::Message as _;
use tokio::net::TcpListener;
use tokio::sync::Notify;

use maho_ai::api::cursor_agent::r#gen::agent_pb::{
    agent_client_message, agent_server_message, conversation_action, exec_client_message, exec_server_message,
    interaction_update, read_result, AgentClientMessage, AgentServerMessage, ConversationAction,
    ExecServerMessage, InteractionUpdate, ReadArgs, ReadSuccess, TextDeltaUpdate, ThinkingDeltaUpdate,
    TokenDeltaUpdate, TurnEndedUpdate,
};
use maho_ai::api::cursor_agent::types::{
    CursorAgentOptions, CursorExecHandlerResult, CursorExecHandlers, CursorToolResultHandler,
};
use maho_ai::api::cursor_agent::{frame_connect_message, stream_with_options};
use maho_ai::types::{
    AssistantMessageEvent, ContentBlock, Context, InputModality, Message, Model, ModelCost, StopReason,
    ToolResultMessage, UserContent, UserMessage,
};

const CONNECT_END_STREAM_FLAG: u8 = 0b0000_0010;

#[derive(Clone, Default)]
struct FrameReader {
    messages: Arc<Mutex<Vec<AgentClientMessage>>>,
    notify: Arc<Notify>,
}

impl FrameReader {
    fn feed(&self, bytes: &[u8]) {
        let message = AgentClientMessage::decode(bytes).expect("client frame decodes");
        self.messages.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(message);
        self.notify.notify_waiters();
    }

    fn find(&self, predicate: impl Fn(&AgentClientMessage) -> bool) -> Option<AgentClientMessage> {
        self.messages
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .find(|message| predicate(message))
            .cloned()
    }

    /// Bounded wait for a frame: `Notify::notified` is armed before the check so a
    /// frame that lands between the check and the wait cannot be missed.
    async fn wait_for<T>(
        &self,
        predicate: impl Fn(&AgentClientMessage) -> Option<T>,
    ) -> T {
        let deadline = tokio::time::sleep(Duration::from_secs(10));
        tokio::pin!(deadline);
        loop {
            let notified = self.notify.notified();
            if let Some(found) = self.find(|message| predicate(message).is_some()).and_then(|message| predicate(&message)) {
                return found;
            }
            tokio::select! {
                _ = &mut deadline => panic!("timed out waiting for a client frame"),
                _ = notified => {}
            }
        }
    }
}

struct ServerStream {
    reader: FrameReader,
    request_headers: http::HeaderMap,
    send: h2::SendStream<Bytes>,
}

impl ServerStream {
    async fn send(&mut self, frame: Vec<u8>) {
        let _ = self.send.send_data(Bytes::from(frame), false);
    }

    fn end(&mut self) {
        let _ = self.send.send_data(Bytes::new(), true);
    }
}

fn server_frame(init: AgentServerMessage) -> Vec<u8> {
    frame_connect_message(&init.encode_to_vec(), 0)
}

fn text_delta_frame(text: &str) -> Vec<u8> {
    server_frame(AgentServerMessage {
        message: Some(agent_server_message::Message::InteractionUpdate(InteractionUpdate {
            message: Some(interaction_update::Message::TextDelta(TextDeltaUpdate { text: text.to_owned() })),
        })),
    })
}

fn thinking_delta_frame(text: &str) -> Vec<u8> {
    server_frame(AgentServerMessage {
        message: Some(agent_server_message::Message::InteractionUpdate(InteractionUpdate {
            message: Some(interaction_update::Message::ThinkingDelta(ThinkingDeltaUpdate { text: text.to_owned() })),
        })),
    })
}

fn token_delta_frame(tokens: i32) -> Vec<u8> {
    server_frame(AgentServerMessage {
        message: Some(agent_server_message::Message::InteractionUpdate(InteractionUpdate {
            message: Some(interaction_update::Message::TokenDelta(TokenDeltaUpdate { tokens })),
        })),
    })
}

fn turn_ended_frame() -> Vec<u8> {
    server_frame(AgentServerMessage {
        message: Some(agent_server_message::Message::InteractionUpdate(InteractionUpdate {
            message: Some(interaction_update::Message::TurnEnded(TurnEndedUpdate::default())),
        })),
    })
}

fn exec_frame(message: exec_server_message::Message) -> Vec<u8> {
    server_frame(AgentServerMessage {
        message: Some(agent_server_message::Message::ExecServerMessage(ExecServerMessage {
            id: 7,
            exec_id: "exec-7".to_owned(),
            message: Some(message),
            ..ExecServerMessage::default()
        })),
    })
}

fn end_stream_error_frame(code: &str, message: &str) -> Vec<u8> {
    let payload = serde_json::json!({ "error": { "code": code, "message": message } });
    frame_connect_message(payload.to_string().as_bytes(), CONNECT_END_STREAM_FLAG)
}

async fn start_server<F, Fut>(handler: F) -> String
where
    F: FnOnce(ServerStream) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("accept");
        let mut connection = server::handshake(socket).await.expect("handshake");
        let Some(Ok((request, mut respond))) = connection.accept().await else { return };
        let request_headers = request.headers().clone();
        let reader = FrameReader::default();
        let body_reader = reader.clone();
        let mut body = request.into_body();
        tokio::spawn(async move {
            let mut pending: Vec<u8> = Vec::new();
            while let Some(chunk) = body.data().await {
                let Ok(chunk) = chunk else { break };
                let _ = body.flow_control().release_capacity(chunk.len());
                pending.extend_from_slice(&chunk);
                while pending.len() >= 5 {
                    let length = u32::from_be_bytes([pending[1], pending[2], pending[3], pending[4]]) as usize;
                    if pending.len() < 5 + length {
                        break;
                    }
                    let frame = pending[5..5 + length].to_vec();
                    pending.drain(..5 + length);
                    body_reader.feed(&frame);
                }
            }
        });
        let send = respond.send_response(http::Response::new(()), false).expect("respond");
        tokio::spawn(async move {
            handler(ServerStream { reader, request_headers, send }).await;
        });
        // The h2 connection future owns the socket IO, so it has to keep being
        // polled while the handler runs in its own task; this loop does that and
        // ends when the client closes.
        while connection.accept().await.is_some() {}
    });
    format!("http://{address}")
}

fn build_model(base_url: &str) -> Model {
    Model {
        id: "claude-4.6-opus-high".to_owned(),
        name: "Opus 4.6".to_owned(),
        api: "cursor-agent".to_owned(),
        provider: "cursor".to_owned(),
        base_url: base_url.to_owned(),
        reasoning: true,
        thinking_level_map: None,
        input: vec![InputModality::Text],
        cost: ModelCost::default(),
        context_window: 200_000,
        max_tokens: 64_000,
        sampling_params: None,
        headers: None,
        cache_retention: None,
        upstream_model_id: None,
        service_tier: None,
        recover_text_tool_calls: None,
        compat: None,
    }
}

fn hello_context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage { content: UserContent::Text("hello".to_owned()), timestamp: 0 })],
        tools: None,
    }
}

async fn collect(base_url: &str, options: CursorAgentOptions, context: Context) -> (Vec<AssistantMessageEvent>, maho_ai::types::AssistantMessage) {
    let stream = stream_with_options(&build_model(base_url), &context, Some(options));
    let events = stream.collect().await.expect("events");
    let message = stream.result().await.expect("result");
    (events, message)
}

fn action_case(action: Option<ConversationAction>) -> Option<String> {
    Some(match action?.action? {
        conversation_action::Action::UserMessageAction(_) => "userMessageAction",
        conversation_action::Action::ResumeAction(_) => "resumeAction",
        _ => "other",
    }
    .to_owned())
}

#[tokio::test]
async fn streams_text_thinking_and_usage_ending_on_turn_ended() {
    let base_url = start_server(|mut stream| async move {
                stream.send(thinking_delta_frame("pondering")).await;
        stream.send(text_delta_frame("Hello ")).await;
        stream.send(text_delta_frame("world")).await;
        stream.send(token_delta_frame(7)).await;
        stream.send(turn_ended_frame()).await;
        stream.end();
    })
    .await;

    let (events, message) = collect(
        &base_url,
        CursorAgentOptions { base: options_with_key("test-token"), ..CursorAgentOptions::default() },
        hello_context(),
    )
    .await;

    assert_eq!(message.stop_reason, StopReason::Stop);
    let kinds: Vec<&str> = message.content.iter().map(ContentBlock::type_name).collect();
    assert_eq!(kinds, vec!["thinking", "text"]);
    match &message.content[0] {
        ContentBlock::Thinking(thinking) => assert_eq!(thinking.thinking, "pondering"),
        other => panic!("expected thinking, got {other:?}"),
    }
    match &message.content[1] {
        ContentBlock::Text(text) => assert_eq!(text.text, "Hello world"),
        other => panic!("expected text, got {other:?}"),
    }
    assert_eq!(message.usage.output, 7);

    let event_types: Vec<&str> = events
        .iter()
        .map(|event| match event {
            AssistantMessageEvent::Start { .. } => "start",
            AssistantMessageEvent::ThinkingStart { .. } => "thinking_start",
            AssistantMessageEvent::ThinkingDelta { .. } => "thinking_delta",
            AssistantMessageEvent::TextStart { .. } => "text_start",
            AssistantMessageEvent::TextDelta { .. } => "text_delta",
            AssistantMessageEvent::TextEnd { .. } => "text_end",
            AssistantMessageEvent::ThinkingEnd { .. } => "thinking_end",
            AssistantMessageEvent::Done { .. } => "done",
            other => panic!("unexpected event {other:?}"),
        })
        .collect();
    assert_eq!(
        event_types,
        vec![
            "start",
            "thinking_start",
            "thinking_delta",
            "text_start",
            "text_delta",
            "text_delta",
            "text_end",
            "thinking_end",
            "done",
        ]
    );
}

fn options_with_key(api_key: &str) -> maho_ai::types::StreamOptions {
    let mut base = maho_ai::types::StreamOptions::default();
    base.request.api_key = Some(api_key.to_owned());
    base
}

#[tokio::test]
async fn sends_protocol_headers_and_sanitizes_caller_supplied_ones() {
    let seen: Arc<Mutex<Option<http::HeaderMap>>> = Arc::new(Mutex::new(None));
    let seen_handler = seen.clone();
    let base_url = start_server(move |mut stream| {
        let seen_handler = seen_handler.clone();
        async move {
            *seen_handler.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(stream.request_headers.clone());
                        stream.send(text_delta_frame("ok")).await;
            stream.send(turn_ended_frame()).await;
            stream.end();
        }
    })
    .await;

    let mut base = options_with_key("test-token");
    base.request.headers = Some(std::collections::BTreeMap::from([
        ("Authorization".to_owned(), Some("Bearer attacker-token".to_owned())),
        ("Connection".to_owned(), Some("keep-alive".to_owned())),
        ("Host".to_owned(), Some("evil.example".to_owned())),
        ("X-Trace-Id".to_owned(), Some("trace-123".to_owned())),
    ]));
    let _ = collect(&base_url, CursorAgentOptions { base, ..CursorAgentOptions::default() }, hello_context()).await;

    let headers = seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone().expect("headers");
    assert_eq!(headers.get("authorization").map(|v| v.to_str().unwrap_or_default()), Some("Bearer test-token"));
    assert_eq!(headers.get("x-trace-id").map(|v| v.to_str().unwrap_or_default()), Some("trace-123"));
    assert_eq!(
        headers.get("content-type").map(|v| v.to_str().unwrap_or_default()),
        Some("application/connect+proto")
    );
    assert_eq!(headers.get("connect-protocol-version").map(|v| v.to_str().unwrap_or_default()), Some("1"));
    assert_eq!(headers.get("x-ghost-mode").map(|v| v.to_str().unwrap_or_default()), Some("true"));
    assert_eq!(headers.get("x-cursor-client-type").map(|v| v.to_str().unwrap_or_default()), Some("cli"));
}

#[tokio::test]
async fn executes_a_server_requested_read_on_the_exec_channel_and_pairs_the_transcript() {
    let base_url = start_server(|mut stream| async move {
                let reader = stream.reader.clone();
        reader
            .wait_for(|message| match message.message.as_ref() {
                Some(agent_client_message::Message::RunRequest(_)) => Some(()),
                _ => None,
            })
            .await;

        stream
            .send(
                exec_frame(exec_server_message::Message::ReadArgs(ReadArgs {
                    path: "src/main.ts".to_owned(),
                    tool_call_id: "call-42".to_owned(),
                    offset: Some(5),
                    limit: Some(10),
                    encoding_hint: None,
                })),
            )
            .await;

        let reply = reader
            .wait_for(|message| match message.message.as_ref() {
                Some(agent_client_message::Message::ExecClientMessage(client))
                    if matches!(client.message, Some(exec_client_message::Message::ReadResult(_))) =>
                {
                    Some(client.clone())
                }
                _ => None,
            })
            .await;
        assert_eq!(reply.id, 7);
        let Some(exec_client_message::Message::ReadResult(result)) = reply.message else {
            panic!("expected a readResult");
        };
        match result.result {
            Some(read_result::Result::Success(ReadSuccess { ref output, range_applied, .. })) => {
                assert!(matches!(output, Some(maho_ai::api::cursor_agent::r#gen::agent_pb::read_success::Output::Content(text)) if text == "file contents here"));
                assert!(range_applied);
            }
            other => panic!("expected a successful readResult, got {other:?}"),
        }

        stream.send(text_delta_frame("done reading")).await;
        stream.send(turn_ended_frame()).await;
        stream.end();
    })
    .await;

    type ReadCall = (String, Option<i32>, Option<u32>);
    let read_calls: Arc<Mutex<Vec<ReadCall>>> = Arc::new(Mutex::new(Vec::new()));
    let paired_results: Arc<Mutex<Vec<ToolResultMessage>>> = Arc::new(Mutex::new(Vec::new()));
    let read_calls_handler = read_calls.clone();
    let paired_handler = paired_results.clone();

    let handlers = CursorExecHandlers {
        read: Some(Arc::new(move |args: ReadArgs| {
            let read_calls = read_calls_handler.clone();
            Box::pin(async move {
                read_calls.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push((
                    args.path.clone(),
                    args.offset,
                    args.limit,
                ));
                CursorExecHandlerResult::ToolResult(ToolResultMessage {
                    tool_call_id: args.tool_call_id.clone(),
                    tool_name: "read".to_owned(),
                    content: vec![ContentBlock::text("file contents here")],
                    details: None,
                    usage: None,
                    added_tool_names: None,
                    is_error: false,
                    timestamp: 0,
                })
            })
        })),
        ..CursorExecHandlers::default()
    };
    let on_tool_result: CursorToolResultHandler = Arc::new(move |result| {
        let paired = paired_handler.clone();
        Box::pin(async move {
            paired.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(result);
            None
        })
    });

    let mut options = CursorAgentOptions { base: options_with_key("test-token"), ..CursorAgentOptions::default() };
    options.exec_handlers = Some(handlers);
    options.on_tool_result = Some(on_tool_result);
    let (_events, message) = collect(&base_url, options, hello_context()).await;

    assert_eq!(
        *read_calls.lock().unwrap_or_else(|poisoned| poisoned.into_inner()),
        vec![("src/main.ts".to_owned(), Some(5), Some(10))]
    );
    let paired = paired_results.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(paired.len(), 1);
    assert_eq!(paired[0].tool_call_id, "call-42");

    let tool_call = message
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::ToolCall(call) => Some(call),
            _ => None,
        })
        .expect("synthesized tool call");
    assert_eq!(tool_call.id, "call-42");
    assert_eq!(tool_call.name, "read");
    assert_eq!(tool_call.arguments.get("path"), Some(&serde_json::json!("src/main.ts")));
    assert_eq!(message.stop_reason, StopReason::Stop);
}

#[tokio::test]
async fn advertises_non_native_tools_through_the_request_context_handshake() {
    let base_url = start_server(|mut stream| async move {
                let reader = stream.reader.clone();
        reader
            .wait_for(|message| match message.message.as_ref() {
                Some(agent_client_message::Message::RunRequest(_)) => Some(()),
                _ => None,
            })
            .await;
        stream
            .send(
                exec_frame(exec_server_message::Message::RequestContextArgs(Default::default())),
            )
            .await;
        let reply = reader
            .wait_for(|message| match message.message.as_ref() {
                Some(agent_client_message::Message::ExecClientMessage(client))
                    if matches!(client.message, Some(exec_client_message::Message::RequestContextResult(_))) =>
                {
                    Some(client.clone())
                }
                _ => None,
            })
            .await;
        let Some(exec_client_message::Message::RequestContextResult(result)) = reply.message else {
            panic!("expected a requestContextResult");
        };
        let context = match result.result {
            Some(maho_ai::api::cursor_agent::r#gen::agent_pb::request_context_result::Result::Success(success)) => {
                success.request_context.expect("request context")
            }
            other => panic!("expected a successful requestContextResult, got {other:?}"),
        };
        assert_eq!(context.tools.iter().map(|tool| tool.name.clone()).collect::<Vec<_>>(), vec!["my_custom_tool".to_owned()]);
        assert_eq!(context.tools[0].provider_identifier, "pi-agent");
        stream.send(turn_ended_frame()).await;
        stream.end();
    })
    .await;

    let mut context = hello_context();
    context.tools = Some(vec![
        maho_ai::types::Tool {
            name: "bash".to_owned(),
            description: "native".to_owned(),
            parameters: serde_json::json!({ "type": "object" }),
            freeform: None,
            constrained_sampling: None,
        },
        maho_ai::types::Tool {
            name: "my_custom_tool".to_owned(),
            description: "custom".to_owned(),
            parameters: serde_json::json!({ "type": "object", "properties": { "value": { "type": "string" } } }),
            freeform: None,
            constrained_sampling: None,
        },
    ]);
    let (_events, message) = collect(
        &base_url,
        CursorAgentOptions { base: options_with_key("test-token"), ..CursorAgentOptions::default() },
        context,
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Stop);
}

#[tokio::test]
async fn surfaces_connect_end_stream_errors() {
    let base_url = start_server(|mut stream| async move {
                stream.send(end_stream_error_frame("resource_exhausted", "quota exceeded")).await;
        stream.end();
    })
    .await;

    let (_events, message) = collect(
        &base_url,
        CursorAgentOptions { base: options_with_key("test-token"), ..CursorAgentOptions::default() },
        hello_context(),
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Error);
    let error = message.error_message.unwrap_or_default();
    assert!(error.contains("Connect error resource_exhausted: quota exceeded"), "{error}");
}

#[tokio::test]
async fn treats_a_stream_that_ends_without_turn_ended_as_incomplete() {
    let base_url = start_server(|mut stream| async move {
                stream.send(text_delta_frame("partial")).await;
        stream.end();
    })
    .await;

    let mut options = CursorAgentOptions { base: options_with_key("test-token"), ..CursorAgentOptions::default() };
    options.stream_stall_max_retries = Some(0);
    let (_events, message) = collect(&base_url, options, hello_context()).await;
    assert_eq!(message.stop_reason, StopReason::Error);
    let error = message.error_message.unwrap_or_default();
    assert!(error.contains("ended before turnEnded"), "{error}");
    let kinds: Vec<&str> = message.content.iter().map(ContentBlock::type_name).collect();
    assert_eq!(kinds, vec!["text"]);
    match &message.content[0] {
        ContentBlock::Text(text) => assert_eq!(text.text, "partial"),
        other => panic!("expected text, got {other:?}"),
    }
}

#[tokio::test]
async fn fails_fast_without_an_access_token() {
    let (_events, message) = collect("http://127.0.0.1:1", CursorAgentOptions::default(), hello_context()).await;
    assert_eq!(message.stop_reason, StopReason::Error);
    let error = message.error_message.unwrap_or_default();
    assert!(error.contains("access token"), "{error}");
}

#[tokio::test]
async fn awaits_payload_hook_before_actual_run_frame_and_ignores_replacement_as_source_does() {
    let (wire_tx, mut wire_rx) = tokio::sync::mpsc::unbounded_channel();
    let base_url = start_server(move |mut server| async move {
        let request = server.reader.wait_for(|message| match &message.message {
            Some(agent_client_message::Message::RunRequest(request)) => Some(request.clone()),
            _ => None,
        }).await;
        wire_tx.send(request).expect("wire run request");
        server.send(turn_ended_frame()).await;
        server.end();
    }).await;
    let (hook_tx, mut hook_rx) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let mut base = options_with_key("test-token");
    base.request.async_on_payload = Some(Arc::new({
        let release = release.clone();
        move |payload, _, _| {
            let tx = hook_tx.clone();
            let release = release.clone();
            Box::pin(async move {
                assert!(payload.get("conversationId").is_some());
                tx.send(()).expect("hook entered");
                release.acquire().await.expect("permit").forget();
                Ok(Some(serde_json::json!({"conversationId":"replacement-must-not-be-used"})))
            })
        }
    }));
    let stream = stream_with_options(&build_model(&base_url), &hello_context(),
        Some(CursorAgentOptions { base, ..Default::default() }));
    assert_eq!(tokio::time::timeout(Duration::from_secs(10), hook_rx.recv()).await.expect("hook timeout"), Some(()));
    assert!(matches!(wire_rx.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
    release.add_permits(1);
    let request = tokio::time::timeout(Duration::from_secs(10), wire_rx.recv()).await.expect("wire timeout").expect("run request");
    assert_ne!(request.conversation_id.as_deref(), Some("replacement-must-not-be-used"));
    assert_eq!(tokio::time::timeout(Duration::from_secs(10), stream.result()).await.expect("result timeout").expect("result").stop_reason, StopReason::Stop);
}

#[tokio::test]
async fn payload_hook_error_and_pending_cancellation_prevent_cursor_connection() {
    for cancel in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("address"));
        let (wire_tx, mut wire_rx) = tokio::sync::mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            let socket = listener.accept().await.expect("accept");
            wire_tx.send(()).expect("connection receiver");
            drop(socket);
        });
        let controller = maho_ai::utils::abort::AbortController::new();
        let (hook_tx, mut hook_rx) = tokio::sync::mpsc::unbounded_channel();
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let mut base = options_with_key("test-token");
        base.request.signal = Some(controller.signal());
        base.request.async_on_payload = Some(Arc::new({
            let release = release.clone();
            move |_, _, _| {
                let tx = hook_tx.clone();
                let release = release.clone();
                Box::pin(async move {
                    tx.send(()).expect("hook entered");
                    release.acquire().await.expect("permit").forget();
                    Err("cursor-payload-exact-error".into())
                })
            }
        }));
        let stream = stream_with_options(&build_model(&url), &hello_context(),
            Some(CursorAgentOptions { base, ..Default::default() }));
        assert_eq!(tokio::time::timeout(Duration::from_secs(10), hook_rx.recv()).await.expect("hook timeout"), Some(()));
        if cancel { controller.abort(None); } else { release.add_permits(1); }
        let result = tokio::time::timeout(Duration::from_secs(10), stream.result()).await.expect("result timeout").expect("result");
        assert!(result.content.is_empty());
        if !cancel { assert_eq!(result.error_message.as_deref(), Some("cursor-payload-exact-error")); }
        assert!(matches!(wire_rx.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
        server.abort();
        assert!(server.await.expect_err("server aborted").is_cancelled());
    }
}

#[tokio::test]
async fn aborts_cleanly_when_the_caller_cancels_mid_stream() {
    let base_url = start_server(|mut stream| async move {
                stream.send(text_delta_frame("started")).await;
        tokio::time::sleep(Duration::from_secs(30)).await;
    })
    .await;

    let controller = maho_ai::utils::abort::AbortController::new();
    let mut base = options_with_key("test-token");
    base.request.signal = Some(controller.signal());
    let options = CursorAgentOptions { base, ..CursorAgentOptions::default() };
    let stream = stream_with_options(&build_model(&base_url), &hello_context(), Some(options));

    let controller_for_task = controller.clone();
    let aborter = {
        let stream = stream.clone();
        tokio::spawn(async move {
            loop {
                let Ok(Some(event)) = stream.next().await else { return };
                if matches!(event, AssistantMessageEvent::TextDelta { .. }) {
                    controller_for_task.abort(None);
                    return;
                }
            }
        })
    };
    let _ = stream.collect().await;
    let message = stream.result().await.expect("result");
    aborter.abort();
    assert_eq!(message.stop_reason, StopReason::Aborted);
}

#[tokio::test]
async fn ignores_a_frame_the_client_never_asked_for() {
    let base_url = start_server(|mut stream| async move {
                stream.send(frame_connect_message(&[0xff, 0xff, 0xff], 0)).await;
        stream.send(turn_ended_frame()).await;
        stream.end();
    })
    .await;

    let (_events, message) = collect(
        &base_url,
        CursorAgentOptions { base: options_with_key("test-token"), ..CursorAgentOptions::default() },
        hello_context(),
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert!(message.content.is_empty());
}

#[tokio::test]
async fn sends_a_user_message_action_for_the_active_turn() {
    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let captured_handler = captured.clone();
    let base_url = start_server(move |mut stream| {
        let captured_handler = captured_handler.clone();
        async move {
                        let reader = stream.reader.clone();
            let request = reader
                .wait_for(|message| match message.message.as_ref() {
                    Some(agent_client_message::Message::RunRequest(_)) => Some(message.clone()),
                    _ => None,
                })
                .await;
            if let Some(agent_client_message::Message::RunRequest(run_request)) = request.message {
                *captured_handler.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
                    action_case(run_request.action);
                let state = run_request.conversation_state.expect("state");
                assert!(!state.root_prompt_messages_json.is_empty());
            }
            stream.send(turn_ended_frame()).await;
            stream.end();
        }
    })
    .await;

    let _ = collect(
        &base_url,
        CursorAgentOptions { base: options_with_key("test-token"), ..CursorAgentOptions::default() },
        hello_context(),
    )
    .await;
    assert_eq!(
        captured.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone(),
        Some("userMessageAction".to_owned())
    );
}
