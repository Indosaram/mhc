//! Agent-level ports of senpi packages/agent/test/internal-events-recovery.test.ts ("delivers the
//! kept attempt's controls and the agent's thinking timing", "a Claude id behind both recovery
//! wrappers"), internal-events-recovery-rewrite.test.ts and
//! internal-events-family-recovery.test.ts (#4272).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use maho_ai::model::Model;
use maho_ai::tool_call_middleware::wrap_stream_with_model_recovery;
use maho_ai::types::{
    AssistantMessage, AssistantMessageEvent, BoxFuture, ContentBlock, DoneReason, ErrorReason,
    InputModality, ProviderNativeContent, StopReason, TextContent, ThinkingContent, Tool, ToolCall,
};
use maho_ai::utils::assistant_capture::{
    AssistantCapture, AssistantProgress, BlockMetadata, BlockPhase, get_assistant_capture,
    push_assistant_progress,
};
use maho_ai::utils::event_stream::AssistantMessageEventStream;
use serde_json::{Value, json};

use super::capture_tests::{attempt, model};
use crate::agent::{Agent, AgentOptions, AgentPromptInput, PartialAgentState};
use crate::internal_events::{InternalEvent, OrderedAgentRecord, create_internal_agent_channel};
use crate::types::{AgentEvent, AgentTool, AgentToolResult, StreamFn};

/// `claude` from internal-events-recovery-fixture.ts: no explicit recovery flag, so the Claude id
/// pattern turns both recovery wrappers on.
fn claude() -> Model {
    let mut claude = crate::agent::default_model();
    claude.id = "claude-sonnet-4-5".to_owned();
    claude.name = "fixture".to_owned();
    claude.api = "anthropic-messages".to_owned();
    claude.provider = "fixture".to_owned();
    claude.base_url = "https://example.invalid".to_owned();
    claude.input = vec![InputModality::Text];
    claude.context_window = 100_000;
    claude.max_tokens = 4096;
    claude
}

fn bash_schema() -> Tool {
    Tool {
        name: "Bash".to_owned(),
        description: "fixture".to_owned(),
        parameters: json!({
            "type": "object",
            "properties": { "command": { "type": "string" } },
            "required": ["command"],
        }),
        output_schema: None,
        freeform: None,
        constrained_sampling: None,
    }
}

fn bash() -> AgentTool {
    AgentTool {
        label: "Bash".to_owned(),
        prepare_arguments: None,
        execute: Arc::new(|_id, _args, _signal, _on_update| {
            Box::pin(async { AgentToolResult::text("ok") })
        }),
        replay: None,
        execution_mode: None,
        tool: bash_schema(),
    }
}

fn message(model: &Model) -> AssistantMessage {
    let mut message = crate::assistant_terminal_state::create_terminal_failure_assistant_message(
        model,
        StopReason::Stop,
        &crate::assistant_terminal_state::AgentStreamError::Other {
            message: String::new(),
        },
        None,
    );
    message.content.clear();
    message.error_message = None;
    message.timestamp = 1;
    message
}

fn text(text: &str) -> ContentBlock {
    ContentBlock::Text(TextContent {
        text: text.to_owned(),
        audience: None,
        text_signature: None,
    })
}

fn web_search(status: &str) -> ContentBlock {
    ContentBlock::ProviderNative(ProviderNativeContent {
        subtype: "web_search_call".to_owned(),
        raw: json!({ "status": status }),
    })
}

/// `claudeTurnWithInvokeInProse(final)`.
fn claude_turn_with_invoke_in_prose(last: bool) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::with_capture_blocks(true);
    let mut output = message(&claude());
    stream.push(AssistantMessageEvent::Start {
        partial: output.clone(),
    });
    if last {
        output.content.push(text("done"));
        stream.push(AssistantMessageEvent::TextStart {
            content_index: 0,
            partial: output.clone(),
        });
        stream.push(AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "done".to_owned(),
            partial: output.clone(),
        });
        stream.push(AssistantMessageEvent::TextEnd {
            content_index: 0,
            content: "done".to_owned(),
            partial: output.clone(),
        });
    } else {
        output
            .content
            .push(ContentBlock::Thinking(ThinkingContent::default()));
        stream.push(AssistantMessageEvent::ThinkingStart {
            content_index: 0,
            partial: output.clone(),
        });
        output.content[0] = ContentBlock::Thinking(ThinkingContent {
            thinking: "plan".to_owned(),
            thinking_signature: Some("sig".to_owned()),
            ..ThinkingContent::default()
        });
        stream.push(AssistantMessageEvent::ThinkingDelta {
            content_index: 0,
            delta: "plan".to_owned(),
            partial: output.clone(),
        });
        push_assistant_progress(
            &stream,
            AssistantProgress::metadata(BlockMetadata {
                content_index: 0,
                thinking_signature_delta: Some("sig".to_owned()),
                ..BlockMetadata::default()
            }),
        );
        stream.push(AssistantMessageEvent::ThinkingEnd {
            content_index: 0,
            content: "plan".to_owned(),
            partial: output.clone(),
        });
        let prose = "before <invoke name=\"Bash\"><parameter name=\"command\">echo hi</parameter></invoke> after";
        output.content.push(text(""));
        stream.push(AssistantMessageEvent::TextStart {
            content_index: 1,
            partial: output.clone(),
        });
        output.content[1] = text(prose);
        stream.push(AssistantMessageEvent::TextDelta {
            content_index: 1,
            delta: prose.to_owned(),
            partial: output.clone(),
        });
        stream.push(AssistantMessageEvent::TextEnd {
            content_index: 1,
            content: prose.to_owned(),
            partial: output.clone(),
        });
        output.content.push(web_search("in_progress"));
        push_assistant_progress(
            &stream,
            AssistantProgress::block_state(2, BlockPhase::Insert, &web_search("in_progress")),
        );
        // TS mutates the same native block object, so the final message holds the completed raw.
        output.content[2] = web_search("completed");
        push_assistant_progress(
            &stream,
            AssistantProgress::block_state(2, BlockPhase::End, &web_search("completed")),
        );
    }
    stream.push(AssistantMessageEvent::Done {
        reason: DoneReason::Stop,
        message: output.clone(),
    });
    stream.end(Some(output));
    stream
}

#[derive(Clone, Copy, PartialEq)]
enum Ending {
    Aborted,
    Stop,
}

/// `nativeCallThenText(ending)` from internal-events-recovery-rewrite.test.ts. Aborted: a native
/// call at 1 before text at 2, and the terminal drops the call, so the text moves to 1.
fn native_call_then_text(ending: Ending) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::with_capture_blocks(true);
    let mut output = message(&claude());
    stream.push(AssistantMessageEvent::Start {
        partial: output.clone(),
    });
    output.content.push(ContentBlock::Thinking(ThinkingContent {
        thinking: "plan".to_owned(),
        thinking_signature: Some("sig".to_owned()),
        ..ThinkingContent::default()
    }));
    stream.push(AssistantMessageEvent::ThinkingStart {
        content_index: 0,
        partial: output.clone(),
    });
    stream.push(AssistantMessageEvent::ThinkingEnd {
        content_index: 0,
        content: "plan".to_owned(),
        partial: output.clone(),
    });
    if ending == Ending::Aborted {
        let call = ToolCall {
            id: "native".to_owned(),
            name: "Bash".to_owned(),
            arguments: serde_json::Map::from_iter([("command".to_owned(), json!("echo"))]),
            ..ToolCall::default()
        };
        output.content.push(ContentBlock::ToolCall(call.clone()));
        stream.push(AssistantMessageEvent::ToolcallStart {
            content_index: 1,
            partial: output.clone(),
        });
        stream.push(AssistantMessageEvent::ToolcallEnd {
            content_index: 1,
            tool_call: call,
            partial: output.clone(),
        });
    }
    let index = output.content.len();
    output.content.push(text("after"));
    stream.push(AssistantMessageEvent::TextStart {
        content_index: index,
        partial: output.clone(),
    });
    stream.push(AssistantMessageEvent::TextDelta {
        content_index: index,
        delta: "after".to_owned(),
        partial: output.clone(),
    });
    stream.push(AssistantMessageEvent::TextEnd {
        content_index: index,
        content: "after".to_owned(),
        partial: output.clone(),
    });
    match ending {
        Ending::Aborted => {
            output.stop_reason = StopReason::Aborted;
            stream.push(AssistantMessageEvent::Error {
                reason: ErrorReason::Aborted,
                error: output.clone(),
            });
        }
        Ending::Stop => {
            output.stop_reason = StopReason::Stop;
            stream.push(AssistantMessageEvent::Done {
                reason: DoneReason::Stop,
                message: output.clone(),
            });
        }
    }
    stream.end(Some(output));
    stream
}

/// Runs one prompt on an Agent built with an internal channel and returns its ordered records.
async fn channel_records(state: PartialAgentState, stream_fn: StreamFn) -> Vec<OrderedAgentRecord> {
    let channel = create_internal_agent_channel();
    let agent = Agent::new(AgentOptions {
        internal_channel: Some(channel.clone()),
        initial_state: Some(state),
        stream_fn: Some(stream_fn),
        ..AgentOptions::default()
    });
    let records: Arc<Mutex<Vec<OrderedAgentRecord>>> = Arc::default();
    let sink = records.clone();
    let unsubscribe = channel.subscribe(Arc::new(move |record| -> BoxFuture<'static, ()> {
        sink.lock().expect("records").push(record);
        Box::pin(async {})
    }));
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        agent.prompt(AgentPromptInput::text("fixture")),
    )
    .await
    .expect("prompt settles");
    unsubscribe();
    std::mem::take(&mut *records.lock().expect("records"))
}

/// `progressOf(records).map(withTimingValuesMasked)`.
fn masked_progress(records: &[OrderedAgentRecord]) -> Vec<Value> {
    records
        .iter()
        .filter_map(|record| match &record.record {
            InternalEvent::Progress(progress) => Some(progress),
            InternalEvent::Event(_) => None,
        })
        .map(|progress| {
            let mut value = serde_json::to_value(&**progress).expect("serialize progress");
            if let AssistantProgress::MessageBlockMetadata(_) = &**progress {
                let map = value.as_object_mut().expect("progress object");
                for key in ["startedAt", "endedAt"] {
                    if let Some(entry) = map.get_mut(key) {
                        *entry = json!("set");
                    }
                }
            }
            value
        })
        .collect()
}

/// The `message_update` events with whether each carries a capture.
fn updates(records: &[OrderedAgentRecord]) -> Vec<(AssistantMessageEvent, bool)> {
    records
        .iter()
        .filter_map(|record| match &record.record {
            InternalEvent::Event(event) => match &event.value {
                AgentEvent::MessageUpdate {
                    assistant_message_event,
                    ..
                } => Some((
                    assistant_message_event.clone(),
                    get_assistant_capture(event).is_some(),
                )),
                _ => None,
            },
            InternalEvent::Progress(_) => None,
        })
        .collect()
}

/// The first assistant `message_end` with its capture, and its index in `records`.
fn first_assistant_end(
    records: &[OrderedAgentRecord],
) -> (usize, AssistantMessage, Option<AssistantCapture>) {
    records
        .iter()
        .enumerate()
        .find_map(|(index, record)| match &record.record {
            InternalEvent::Event(event) => match &event.value {
                AgentEvent::MessageEnd { message } => message.as_assistant().map(|assistant| {
                    (
                        index,
                        assistant.clone(),
                        get_assistant_capture(event).cloned(),
                    )
                }),
                _ => None,
            },
            InternalEvent::Progress(_) => None,
        })
        .expect("no assistant message_end")
}

fn block_types(message: &AssistantMessage) -> Vec<&'static str> {
    message
        .content
        .iter()
        .map(ContentBlock::type_name)
        .collect()
}

// internal-events-recovery.test.ts
#[tokio::test]
async fn delivers_the_kept_attempts_controls_and_the_agents_thinking_timing_to_the_internal_channel()
 {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let stream_fn: StreamFn = Arc::new(move |_, _, _| {
        if count.fetch_add(1, Ordering::SeqCst) == 0 {
            attempt("discard-me", "")
        } else {
            attempt("kept", "kept")
        }
    });
    let records = channel_records(
        PartialAgentState {
            model: Some(model()),
            ..PartialAgentState::default()
        },
        stream_fn,
    )
    .await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        masked_progress(&records),
        vec![
            json!({ "type": "message_block_metadata", "contentIndex": 0, "startedAt": "set" }),
            json!({ "type": "message_block_metadata", "contentIndex": 0, "thinkingSignatureDelta": "kept" }),
            json!({ "type": "message_block_metadata", "contentIndex": 0, "startedAt": "set", "endedAt": "set" }),
        ]
    );
    let updates = updates(&records);
    assert!(!updates.is_empty());
    assert!(updates.iter().all(|(_, captured)| *captured));
}

#[tokio::test]
async fn a_claude_id_behind_both_recovery_wrappers_receives_its_records_at_the_indices_of_the_message_it_reads()
 {
    let turn = Arc::new(AtomicUsize::new(0));
    let count = turn.clone();
    let stream_fn: StreamFn = Arc::new(move |_, _, _| {
        let last = count.fetch_add(1, Ordering::SeqCst) + 1 > 1;
        wrap_stream_with_model_recovery(
            claude_turn_with_invoke_in_prose(last),
            &claude(),
            &[bash_schema()],
        )
    });
    let records = channel_records(
        PartialAgentState {
            model: Some(claude()),
            tools: Some(vec![bash()]),
            ..PartialAgentState::default()
        },
        stream_fn,
    )
    .await;
    assert_eq!(turn.load(Ordering::SeqCst), 2);
    let (first_end, end, capture) = first_assistant_end(&records);
    let first_turn = &records[..=first_end];
    assert_eq!(
        block_types(&end),
        vec!["thinking", "text", "toolCall", "text", "providerNative"]
    );
    assert_eq!(
        masked_progress(first_turn),
        vec![
            json!({ "type": "message_block_metadata", "contentIndex": 0, "startedAt": "set" }),
            json!({ "type": "message_block_metadata", "contentIndex": 0, "thinkingSignatureDelta": "sig" }),
            json!({ "type": "message_block_metadata", "contentIndex": 0, "startedAt": "set", "endedAt": "set" }),
            json!({
                "type": "message_block_state",
                "contentIndex": 4,
                "phase": "insert",
                "block": { "type": "providerNative", "subtype": "web_search_call", "raw": { "status": "in_progress" } },
            }),
            json!({
                "type": "message_block_state",
                "contentIndex": 4,
                "phase": "end",
                "block": { "type": "providerNative", "subtype": "web_search_call", "raw": { "status": "completed" } },
            }),
        ]
    );
    let updates = updates(first_turn);
    let shapes: Vec<String> = updates
        .iter()
        .map(|(event, _)| {
            let value = serde_json::to_value(event).expect("serialize event");
            let index = value
                .get("contentIndex")
                .map(Value::to_string)
                .unwrap_or_default();
            format!("{}:{index}", value["type"].as_str().expect("event type"))
        })
        .collect();
    assert_eq!(
        shapes,
        vec![
            "thinking_start:0",
            "thinking_delta:0",
            "thinking_end:0",
            "text_start:1",
            "text_delta:1",
            "toolcall_start:2",
            "toolcall_delta:2",
            "toolcall_end:2",
            "text_delta:3",
            "text_end:3",
        ]
    );
    assert!(updates.iter().all(|(_, captured)| *captured));
    assert_eq!(
        capture
            .and_then(|capture| capture.message)
            .map(|message| block_types(&message)),
        Some(block_types(&end))
    );
}

// internal-events-recovery-rewrite.test.ts
async fn assistant_end(ending: Ending) -> (AssistantMessage, Option<AssistantCapture>) {
    let stream_fn: StreamFn = Arc::new(move |_, _, _| {
        wrap_stream_with_model_recovery(native_call_then_text(ending), &claude(), &[bash_schema()])
    });
    let records = channel_records(
        PartialAgentState {
            model: Some(claude()),
            tools: Some(vec![bash()]),
            ..PartialAgentState::default()
        },
        stream_fn,
    )
    .await;
    let (_, message, capture) = first_assistant_end(&records);
    (message, capture)
}

#[tokio::test]
async fn carries_rewritten_from_through_empty_assistant_recovery_and_the_loop_onto_message_end() {
    let (message, capture) = assistant_end(Ending::Aborted).await;
    assert_eq!(block_types(&message), vec!["thinking", "text"]);
    assert_eq!(capture.and_then(|capture| capture.rewritten_from), Some(1));
}

#[tokio::test]
async fn marks_no_rewrite_on_the_message_end_of_a_completed_turn() {
    let (_, capture) = assistant_end(Ending::Stop).await;
    let capture = capture.expect("message_end carries a capture");
    assert_eq!(capture.rewritten_from, None);
}

// internal-events-family-recovery.test.ts: a stub Cascade edge (GetUserJwt, then the chat
// stream) answering every request with the same two frames and the trailer.
mod family {
    use super::*;

    use maho_ai::api::devin_agent::r#gen::cascade_pb::GetChatMessageResponse;
    use maho_ai::api::devin_agent::wire::{DEVIN_USER_JWT_PATH, encode_devin_request_frame};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// `GetUserJwtResponse { userJwt: "jwt-1" }` as bare protobuf (field 1, length-delimited).
    fn user_jwt_response() -> Vec<u8> {
        let mut body = vec![0x0a, 5];
        body.extend_from_slice(b"jwt-1");
        body
    }

    fn chat_body() -> Vec<u8> {
        let mut body = encode_devin_request_frame(&GetChatMessageResponse {
            message_id: "m1".to_owned(),
            delta_thinking: "plan".to_owned(),
            delta_signature: "s1".to_owned(),
            ..GetChatMessageResponse::default()
        });
        body.extend(encode_devin_request_frame(&GetChatMessageResponse {
            message_id: "m1".to_owned(),
            delta_text: "done".to_owned(),
            ..GetChatMessageResponse::default()
        }));
        let trailer = b"{\"metadata\":{}}";
        body.push(0x02);
        body.extend_from_slice(&(trailer.len() as u32).to_be_bytes());
        body.extend_from_slice(trailer);
        body
    }

    async fn respond(mut socket: tokio::net::TcpStream) {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut head: Option<(usize, usize)> = None;
        loop {
            if let Some((end, length)) = head
                && buffer.len() >= end + length
            {
                break;
            }
            let read = socket.read(&mut chunk).await.unwrap_or(0);
            if read == 0 {
                return;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if head.is_none()
                && let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n")
            {
                let text = String::from_utf8_lossy(&buffer[..end]).to_ascii_lowercase();
                let length = text
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse().ok())
                    .unwrap_or(0);
                head = Some((end + 4, length));
            }
        }
        let request_line = String::from_utf8_lossy(&buffer)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned();
        let path = request_line.split(' ').nth(1).unwrap_or_default();
        let (content_type, body) = if path == DEVIN_USER_JWT_PATH {
            ("application/proto", user_jwt_response())
        } else {
            ("application/connect+proto", chat_body())
        };
        let header = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        let _ = socket.write_all(header.as_bytes()).await;
        let _ = socket.write_all(&body).await;
        let _ = socket.shutdown().await;
    }

    struct Edge {
        base_url: String,
        server: tokio::task::JoinHandle<()>,
    }

    impl Drop for Edge {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    async fn serve_edge() -> Edge {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub edge");
        let base_url = format!("http://{}", listener.local_addr().expect("stub edge addr"));
        let server = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(respond(socket));
            }
        });
        Edge { base_url, server }
    }

    async fn channel(model_id: &str) -> Vec<OrderedAgentRecord> {
        let edge = serve_edge().await;
        let mut model = crate::agent::default_model();
        model.id = model_id.to_owned();
        model.name = "SWE-1.6".to_owned();
        model.api = "devin-agent".to_owned();
        model.provider = "devin".to_owned();
        model.base_url = edge.base_url.clone();
        model.reasoning = true;
        model.input = vec![InputModality::Text];
        model.context_window = 200_000;
        model.max_tokens = 128_000;
        let devin = model.clone();
        let stream_fn: StreamFn = Arc::new(move |_model, context, options| {
            let mut options = options
                .map(|options| options.simple.stream)
                .unwrap_or_default();
            options.request.api_key = Some("session-abc".to_owned());
            maho_ai::api::devin_agent::stream(&devin, context, Some(options))
        });
        channel_records(
            PartialAgentState {
                model: Some(model),
                ..PartialAgentState::default()
            },
            stream_fn,
        )
        .await
    }

    fn record_shapes(records: &[OrderedAgentRecord]) -> Vec<String> {
        records
            .iter()
            .map(|record| match &record.record {
                InternalEvent::Progress(progress) => {
                    let mut value = serde_json::to_value(&**progress).expect("serialize progress");
                    let map = value.as_object_mut().expect("progress object");
                    let started = map.contains_key("startedAt");
                    let ended = map.contains_key("endedAt");
                    map.retain(|key, _| key != "startedAt" && key != "endedAt");
                    let timing = format!(
                        "{}{}",
                        if started { "startedAt" } else { "" },
                        if ended { "endedAt" } else { "" }
                    );
                    format!("progress {value} {timing}")
                }
                InternalEvent::Event(event) => {
                    let value = serde_json::to_value(&event.value).expect("serialize event");
                    let kind = value["type"].as_str().expect("event type").to_owned();
                    let kind = match &event.value {
                        AgentEvent::MessageUpdate { .. } => format!(
                            "{kind}:{}",
                            value["assistantMessageEvent"]["type"]
                                .as_str()
                                .expect("assistant event type")
                        ),
                        _ => kind,
                    };
                    format!("{kind} carrier={}", get_assistant_capture(event).is_some())
                }
            })
            .collect()
    }

    #[tokio::test]
    async fn delivers_a_claude_ids_devin_records_exactly_as_an_unwrapped_ids() {
        let signature = format!(
            "progress {} ",
            json!({ "type": "message_block_metadata", "contentIndex": 0, "thinkingSignature": "s1" })
        );
        let unwrapped = record_shapes(&channel("swe-1-6").await);
        assert!(unwrapped.contains(&signature));
        assert!(unwrapped.contains(&"message_update:thinking_start carrier=true".to_owned()));
        let wrapped = record_shapes(&channel("claude-sonnet-4-5").await);
        // The Devin fixture's first frame opens the thinking block, so its signature names block 0
        // behind the wrapper too.
        assert!(wrapped.contains(&signature));
        assert_eq!(wrapped, unwrapped);
    }
}
