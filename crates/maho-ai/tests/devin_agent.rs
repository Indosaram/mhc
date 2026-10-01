//! Ports of the devin-agent TS suites (todos 12): `test/devin-agent-wire.test.ts`,
//! `test/devin-agent-request.test.ts`, `test/devin-agent-stream.test.ts` and
//! `test/devin-agent-stream-deltas.test.ts`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use flate2::Compression;
use flate2::write::GzEncoder;
use maho_ai::api::devin_agent::r#gen::cascade_pb::{
    AssignModelRequest, AssignModelResponse, ChatMessageSource, GetChatMessageRequest, GetChatMessageResponse,
    GetUserJwtRequest, GetUserJwtResponse, ModelAssignment, StopReason,
};
use maho_ai::api::devin_agent::wire::{
    build_devin_chat_request, build_devin_router_prompt, decode_devin_frames, devin_cli_metadata,
    devin_discovery_metadata, encode_devin_request_frame, normalize_devin_session_token, read_devin_trailer_error,
    DevinChatRequestInput, DevinFrame, DEVIN_ASSIGN_MODEL_PATH, DEVIN_CHAT_MESSAGE_PATH, DEVIN_CLI_MODEL_CONFIGS_PATH,
    DEVIN_USER_JWT_PATH,
};
use maho_ai::types::{
    AssistantMessageEvent, Context, ContentBlock, ImageContent, Message, Model, ModelCost, StopReason as MessageStopReason,
    StreamOptions, Tool, ToolCall, UserContent, UserMessage,
};
use maho_ai::utils::abort::{AbortController, AbortReason};
use prost::Message as ProstMessage;

const UUID_SHAPE_LEN: usize = 36;

fn model(base_url: &str) -> Model {
    Model {
        id: "swe-1-6".into(),
        name: "SWE-1.6".into(),
        api: "devin-agent".into(),
        provider: "devin".into(),
        base_url: base_url.into(),
        reasoning: true,
        thinking_level_map: None,
        input: vec![maho_ai::types::InputModality::Text],
        cost: ModelCost::default(),
        context_window: 200_000,
        max_tokens: 128_000,
        sampling_params: None,
        headers: None,
        cache_retention: None,
        upstream_model_id: None,
        service_tier: None,
        recover_text_tool_calls: None,
        compat: None,
    }
}

fn context() -> Context {
    Context {
        system_prompt: Some("sys".into()),
        messages: vec![Message::User(UserMessage { content: UserContent::Text("hi".into()), timestamp: 0 })],
        tools: None,
    }
}

fn wire_context() -> Context {
    Context {
        system_prompt: Some("You are senpi.\n\nBe precise.".into()),
        messages: vec![Message::User(UserMessage { content: UserContent::Text("hello".into()), timestamp: 0 })],
        tools: None,
    }
}

/// Locates one length-delimited protobuf field by tag in an encoded message; `None` when absent.
fn find_string_field(bytes: &[u8], field_number: u32) -> Option<String> {
    let mut offset = 0usize;
    let read_varint = |offset: &mut usize| -> Option<u64> {
        let mut value = 0u64;
        let mut shift = 0u32;
        loop {
            let byte = *bytes.get(*offset)?;
            *offset += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
            shift += 7;
        }
    };
    while offset < bytes.len() {
        let tag = read_varint(&mut offset)?;
        let wire_type = tag & 0x7;
        let number = (tag >> 3) as u32;
        match wire_type {
            0 => {
                read_varint(&mut offset)?;
            }
            2 => {
                let length = read_varint(&mut offset)? as usize;
                let slice = bytes.get(offset..offset + length)?;
                offset += length;
                if number == field_number {
                    return Some(String::from_utf8_lossy(slice).into_owned());
                }
            }
            1 => offset += 8,
            5 => offset += 4,
            _ => return None,
        }
    }
    None
}

fn is_uuid_shape(value: &str) -> bool {
    value.len() == UUID_SHAPE_LEN
        && value.split('-').map(str::len).collect::<Vec<_>>() == vec![8, 4, 4, 4, 12]
        && value.chars().all(|character| character.is_ascii_hexdigit() || character == '-')
}

// ---------------------------------------------------------------- wire

#[test]
fn pins_the_cascade_rpc_paths_the_released_cli_calls() {
    assert_eq!(DEVIN_CHAT_MESSAGE_PATH, "/exa.api_server_pb.ApiServerService/GetChatMessage");
    assert_eq!(DEVIN_CLI_MODEL_CONFIGS_PATH, "/exa.api_server_pb.ApiServerService/GetCliModelConfigs");
    assert_eq!(DEVIN_USER_JWT_PATH, "/exa.auth_pb.AuthService/GetUserJwt");
    assert_eq!(DEVIN_ASSIGN_MODEL_PATH, "/exa.api_server_pb.ApiServerService/AssignModel");
}

#[test]
fn prefixes_the_session_token_exactly_once() {
    assert_eq!(normalize_devin_session_token(Some("abc")), "devin-session-token$abc");
    assert_eq!(normalize_devin_session_token(Some("devin-session-token$abc")), "devin-session-token$abc");
    assert_eq!(normalize_devin_session_token(None), "");
}

#[test]
fn carries_the_released_cli_identity_and_the_prefixed_token_in_chat_metadata() {
    let metadata = devin_cli_metadata(Some("abc"), "jwt-1");
    assert_eq!(metadata.api_key, "devin-session-token$abc");
    assert_eq!(metadata.user_jwt, "jwt-1");
    assert_eq!(metadata.ide_name, "devin-cli");
    assert_eq!(metadata.ide_type, "chisel");
    assert_eq!(metadata.ide_version, "3000.6.2");
    assert_eq!(metadata.extension_name, "chisel");
    assert_eq!(metadata.extension_version, "3000.6.2");
    assert_eq!(metadata.locale, "en");
    assert!(["darwin", "linux", "windows"].contains(&metadata.os.as_str()));
    assert_eq!(devin_cli_metadata(Some("abc"), "").user_jwt, "");
}

#[test]
fn encodes_the_user_jwt_on_metadata_field_21_never_on_22() {
    let bytes = devin_cli_metadata(Some("abc"), "jwt-1").encode_to_vec();
    assert_eq!(find_string_field(&bytes, 21).as_deref(), Some("jwt-1"));
    assert_eq!(find_string_field(&bytes, 22), None);
}

#[test]
fn announces_the_dev_channel_chisel_identity_with_the_native_display_slots_for_discovery() {
    let metadata = devin_discovery_metadata(Some("abc"));
    assert_eq!(metadata.api_key, "devin-session-token$abc");
    assert_eq!(metadata.ide_name, "chisel");
    assert_eq!(metadata.ide_version, "0.0.0-dev");
    assert_eq!(metadata.extension_name, "chisel");
    assert_eq!(metadata.extension_version, "0.0.0-dev");
    assert_eq!(metadata.locale, "en");
    assert_eq!(metadata.ide_type, "");
    assert_eq!(metadata.supported_model_displays, vec![3, 4, 6, 7, 8]);
}

#[tokio::test]
async fn frames_a_request_as_one_gzipped_connect_frame() {
    let context = wire_context();
    let model = model("https://server.codeium.com");
    let request = build_devin_chat_request(&DevinChatRequestInput {
        model: &model,
        context: &context,
        api_key: Some("abc"),
        user_jwt: None,
        cascade_id: "c",
        assignment: None,
        max_tokens: None,
        temperature: None,
        top_p: None,
        stop_sequences: None,
    });
    let frame = encode_devin_request_frame(&request);

    assert_eq!(frame[0], 0x01);
    let length = u32::from_be_bytes([frame[1], frame[2], frame[3], frame[4]]);
    assert_eq!(length as usize, frame.len() - 5);

    let mut decoded: Vec<DevinFrame<GetChatMessageRequest>> = Vec::new();
    decode_devin_frames(one_shot(frame), |item| {
        decoded.push(item);
        Ok(())
    })
    .await
    .expect("frames decode");
    let DevinFrame::Message(message) = &decoded[0] else { panic!("expected a message frame") };
    assert_eq!(message.prompt, "You are senpi.\n\nBe precise.");
}

#[tokio::test]
async fn decodes_a_streamed_response_and_surfaces_the_end_of_stream_trailer() {
    let first = frame_of(GetChatMessageResponse { message_id: "m1".into(), delta_text: "he".into(), ..Default::default() });
    let second = frame_of(GetChatMessageResponse {
        message_id: "m1".into(),
        delta_text: "llo".into(),
        stop_reason: StopReason::FunctionCall as i32,
        ..Default::default()
    });
    let trailer = trailer_frame_with("{\"metadata\":{}}");

    let mut decoded: Vec<DevinFrame<GetChatMessageResponse>> = Vec::new();
    let mut parts = first;
    parts.extend_from_slice(&second);
    parts.extend_from_slice(&trailer);
    decode_devin_frames(one_shot(parts), |item| {
        decoded.push(item);
        Ok(())
    })
    .await
    .expect("frames decode");

    let deltas: Vec<Option<String>> = decoded
        .iter()
        .map(|frame| match frame {
            DevinFrame::Message(message) => Some(message.delta_text.clone()),
            DevinFrame::Trailer(_) => None,
        })
        .collect();
    assert_eq!(deltas, vec![Some("he".into()), Some("llo".into()), None]);
    let Some(DevinFrame::Trailer(trailer)) = decoded.last() else { panic!("expected a trailer frame") };
    assert_eq!(trailer, "{\"metadata\":{}}");
}

#[test]
fn reads_a_connect_error_trailer_and_ignores_a_clean_one() {
    assert!(read_devin_trailer_error("{\"metadata\":{}}").is_none());
    assert!(read_devin_trailer_error("").is_none());
    assert!(read_devin_trailer_error("not json").is_none());
    let error = read_devin_trailer_error(
        "{\"error\":{\"code\":\"invalid_argument\",\"message\":\"an internal error occurred\",\"details\":[{\"type\":\"t\",\"debug\":{\"x\":1}}]}}",
    )
    .expect("trailer error");
    assert_eq!(error.code, "invalid_argument");
    assert_eq!(error.message, "an internal error occurred");
    assert!(error.formatted.contains("invalid_argument"));
    assert!(error.formatted.contains("an internal error occurred"));
    assert!(error.formatted.contains("t: {\"x\":1}"));
}

#[tokio::test]
async fn rejects_a_frame_whose_length_prefix_exceeds_the_payload_cap() {
    let mut bogus = vec![0u8; 5];
    bogus[1..5].copy_from_slice(&0xffff_ffffu32.to_be_bytes());
    let error = decode_devin_frames::<GetChatMessageResponse, _, _>(one_shot(bogus), |_| Ok(()))
        .await
        .expect_err("a hostile length prefix must be rejected");
    assert!(error.to_string().contains("cap"), "unexpected error: {error}");
}

// ---------------------------------------------------------------- request

fn request_context() -> Context {
    Context {
        system_prompt: Some("You are senpi.\n\nBe precise.".into()),
        messages: vec![
            Message::User(UserMessage {
                content: UserContent::Blocks(vec![ContentBlock::Text(maho_ai::types::TextContent {
                    text: "hello".into(),
                    audience: None,
                    text_signature: None,
                })]),
                timestamp: 0,
            }),
            Message::Assistant(Box::new(foreign_assistant_message(vec![
                ContentBlock::Thinking(maho_ai::types::ThinkingContent {
                    thinking: "considering".into(),
                    started_at: None,
                    ended_at: None,
                    thinking_signature: Some("sig-1".into()),
                    redacted: None,
                }),
                ContentBlock::Text(maho_ai::types::TextContent { text: "hi".into(), audience: None, text_signature: None }),
                ContentBlock::ToolCall(ToolCall {
                    id: "call-1".into(),
                    name: "read".into(),
                    arguments: [("path".to_owned(), serde_json::json!("a.ts"))].into_iter().collect(),
                    incomplete: None,
                    error_message: None,
                    thought_signature: None,
                    namespace: None,
                }),
            ]))),
            Message::ToolResult(maho_ai::types::ToolResultMessage {
                tool_call_id: "call-1".into(),
                tool_name: "read".into(),
                content: vec![ContentBlock::Text(maho_ai::types::TextContent {
                    text: "file body".into(),
                    audience: None,
                    text_signature: None,
                })],
                details: None,
                usage: None,
                added_tool_names: None,
                is_error: false,
                timestamp: 0,
            }),
        ],
        tools: Some(vec![Tool {
            name: "read".into(),
            description: "Read a file".into(),
            parameters: serde_json::json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
            freeform: None,
            constrained_sampling: None,
        }]),
    }
}

/// An assistant turn produced by another provider: senpi's fixture leaves api/provider/model
/// unset, which is what makes the replay non-native.
fn foreign_assistant_message(content: Vec<ContentBlock>) -> maho_ai::types::AssistantMessage {
    let mut message = assistant_message(content);
    message.api = "other-api".into();
    message.provider = "other-provider".into();
    message.model = "other-model".into();
    message
}

fn assistant_message(content: Vec<ContentBlock>) -> maho_ai::types::AssistantMessage {
    maho_ai::types::AssistantMessage {
        content,
        api: "devin-agent".into(),
        provider: "devin".into(),
        model: "swe-1-6".into(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        diagnostics: None,
        usage: Default::default(),
        stop_reason: MessageStopReason::Stop,
        stop_details: None,
        deferred: None,
        error_message: None,
        abort_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
    }
}

fn build_request(model: &Model, context: &Context, assignment: Option<&maho_ai::api::devin_agent::wire::DevinModelAssignment>) -> GetChatMessageRequest {
    build_devin_chat_request(&DevinChatRequestInput {
        model,
        context,
        api_key: Some("abc"),
        user_jwt: Some("jwt-1"),
        cascade_id: "conv-1",
        assignment,
        max_tokens: None,
        temperature: None,
        top_p: None,
        stop_sequences: None,
    })
}

#[test]
fn flattens_the_system_prompt_and_orders_history_with_cascade_roles() {
    let request = build_request(&model("https://server.codeium.com"), &request_context(), None);

    assert_eq!(request.prompt, "You are senpi.\n\nBe precise.");
    assert_eq!(request.chat_model_uid, "swe-1-6");
    assert_eq!(request.chat_model_name, "");
    assert_eq!(request.cascade_id, "conv-1");
    assert!(is_uuid_shape(&request.execution_id));
    let metadata = request.metadata.as_ref().expect("metadata");
    assert_eq!(metadata.api_key, "devin-session-token$abc");
    assert_eq!(metadata.user_jwt, "jwt-1");
    assert_eq!(metadata.ide_name, "devin-cli");
    let sources: Vec<i32> = request.chat_message_prompts.iter().map(|prompt| prompt.source).collect();
    assert_eq!(sources, vec![1, 2, 4]);
    assert_eq!(request.chat_message_prompts[0].prompt, "hello");
    assert_eq!(request.chat_message_prompts[1].thinking, "considering");
    assert_eq!(request.chat_message_prompts[1].signature, "");
    let tool_call = &request.chat_message_prompts[1].tool_calls[0];
    assert_eq!(tool_call.id, "call-1");
    assert_eq!(tool_call.name, "read");
    assert_eq!(tool_call.arguments_json, "{\"path\":\"a.ts\"}");
    assert_eq!(request.chat_message_prompts[2].tool_call_id, "call-1");
    assert_eq!(request.chat_message_prompts[2].prompt, "file body");
    assert!(!request.chat_message_prompts[2].tool_result_is_error);
    let tool = &request.tools[0];
    assert_eq!(tool.name, "read");
    assert_eq!(tool.description, "Read a file");
    assert!(!tool.strict);
    let schema: serde_json::Value = serde_json::from_str(&tool.json_schema_string).expect("schema json");
    assert_eq!(schema["type"], "object");
    assert_eq!(request.system_prompt_cache_options.as_ref().expect("cache options").r#type, 1);
    assert!(request.disable_parallel_tool_calls);
    assert!(request.model_assignment_jwt.is_none());
}

#[test]
fn mints_uuid_shaped_message_ids_that_are_stable_per_cascade_and_index() {
    let model = model("https://server.codeium.com");
    let first = build_request(&model, &request_context(), None);
    let again = build_request(&model, &request_context(), None);
    let other = build_devin_chat_request(&DevinChatRequestInput {
        model: &model,
        context: &request_context(),
        api_key: Some("abc"),
        user_jwt: None,
        cascade_id: "conv-2",
        assignment: None,
        max_tokens: None,
        temperature: None,
        top_p: None,
        stop_sequences: None,
    });

    let ids: Vec<String> = first.chat_message_prompts.iter().map(|prompt| prompt.message_id.clone()).collect();
    assert!(is_uuid_shape(&ids[0]));
    assert!(ids[1].starts_with("bot-"));
    assert!(is_uuid_shape(ids[1].trim_start_matches("bot-")));
    assert!(is_uuid_shape(&ids[2]));
    let again_ids: Vec<String> = again.chat_message_prompts.iter().map(|prompt| prompt.message_id.clone()).collect();
    assert_eq!(again_ids, ids);
    let other_ids: Vec<String> = other.chat_message_prompts.iter().map(|prompt| prompt.message_id.clone()).collect();
    assert_ne!(other_ids, ids);
    assert_ne!(first.execution_id, again.execution_id);
}

#[test]
fn replays_a_native_devin_assistant_turn_under_its_own_response_id_and_thinking_signature() {
    let mut native = assistant_message(vec![
        ContentBlock::Thinking(maho_ai::types::ThinkingContent {
            thinking: "t".into(),
            started_at: None,
            ended_at: None,
            thinking_signature: Some("sig-native".into()),
            redacted: None,
        }),
        ContentBlock::Text(maho_ai::types::TextContent { text: "yo".into(), audience: None, text_signature: None }),
    ]);
    native.response_id = Some("msg-native-1".into());
    let context = Context {
        system_prompt: Some("sys".into()),
        messages: vec![
            Message::User(UserMessage { content: UserContent::Text("hi".into()), timestamp: 0 }),
            Message::Assistant(Box::new(native)),
            Message::Assistant(Box::new(assistant_message(Vec::new()))),
        ],
        tools: None,
    };
    let request = build_request(&model("https://server.codeium.com"), &context, None);
    assert_eq!(request.chat_message_prompts.len(), 2);
    let replayed = &request.chat_message_prompts[1];
    assert_eq!(replayed.message_id, "msg-native-1");
    assert_eq!(replayed.signature, "sig-native");
    assert_eq!(replayed.prompt, "yo");
}

#[test]
fn carries_inline_images_on_user_prompts_and_tool_results() {
    let context = Context {
        system_prompt: Some("sys".into()),
        messages: vec![
            Message::User(UserMessage {
                content: UserContent::Blocks(vec![
                    ContentBlock::Text(maho_ai::types::TextContent { text: "look".into(), audience: None, text_signature: None }),
                    ContentBlock::Image(ImageContent { data: "AAAA".into(), mime_type: "image/png".into() }),
                ]),
                timestamp: 0,
            }),
            Message::ToolResult(maho_ai::types::ToolResultMessage {
                tool_call_id: "call-1".into(),
                tool_name: "shot".into(),
                content: vec![ContentBlock::Image(ImageContent { data: "BBBB".into(), mime_type: "image/jpeg".into() })],
                details: None,
                usage: None,
                added_tool_names: None,
                is_error: true,
                timestamp: 0,
            }),
        ],
        tools: None,
    };
    let request = build_request(&model("https://server.codeium.com"), &context, None);
    assert_eq!(request.chat_message_prompts[0].prompt, "look");
    assert_eq!(request.chat_message_prompts[0].images[0].base64_data, "AAAA");
    assert_eq!(request.chat_message_prompts[0].images[0].mime_type, "image/png");
    assert_eq!(request.chat_message_prompts[1].images[0].base64_data, "BBBB");
    assert_eq!(request.chat_message_prompts[1].images[0].mime_type, "image/jpeg");
    assert!(request.chat_message_prompts[1].tool_result_is_error);
}

#[test]
fn sends_the_released_cli_completion_configuration_and_never_a_zero_temperature() {
    let model = model("https://server.codeium.com");
    let request = build_request(&model, &request_context(), None);
    let configuration = request.configuration.as_ref().expect("configuration");
    assert_eq!(configuration.num_completions, 1);
    assert_eq!(configuration.max_tokens, 128_000);
    assert_eq!(configuration.max_newlines, 200);
    assert_eq!(configuration.temperature, 0.4);
    assert_eq!(configuration.first_temperature, 0.4);
    assert_eq!(configuration.top_k, 50);
    assert_eq!(configuration.top_p, 1.0);
    assert_eq!(configuration.fim_eot_prob_threshold, 1.0);
    assert_eq!(
        configuration.stop_patterns,
        vec!["<|user|>", "<|bot|>", "<|context_request|>", "<|endoftext|>", "<|end_of_turn|>"]
    );

    let tuned = build_devin_chat_request(&DevinChatRequestInput {
        model: &model,
        context: &request_context(),
        api_key: Some("abc"),
        user_jwt: None,
        cascade_id: "c",
        assignment: None,
        max_tokens: Some(4096),
        temperature: Some(0.0),
        top_p: Some(0.9),
        stop_sequences: Some(&["END".to_owned()]),
    });
    let tuned = tuned.configuration.expect("configuration");
    assert_eq!(tuned.max_tokens, 4096);
    assert!(tuned.temperature > 0.0);
    assert_eq!(tuned.first_temperature, tuned.temperature);
    assert_eq!(tuned.top_p, 0.9);
    assert_eq!(
        tuned.stop_patterns,
        vec!["<|user|>", "<|bot|>", "<|context_request|>", "<|endoftext|>", "<|end_of_turn|>", "END"]
    );
}

#[test]
fn honors_the_upstream_wire_id_parallel_tool_calls_and_a_router_assignment() {
    let mut routed = model("https://server.codeium.com");
    routed.id = "adaptive".into();
    routed.upstream_model_id = Some("adaptive-wire".into());
    routed.compat = Some(maho_ai::model::ModelCompat(
        [
            ("modelRouter".to_owned(), serde_json::json!(true)),
            ("supportsParallelToolCalls".to_owned(), serde_json::json!(true)),
        ]
        .into_iter()
        .collect(),
    ));
    let plain = build_request(&routed, &request_context(), None);
    assert_eq!(plain.chat_model_uid, "adaptive-wire");
    assert!(!plain.disable_parallel_tool_calls);

    let assignment = maho_ai::api::devin_agent::wire::DevinModelAssignment {
        model_uid: "claude-sonnet-5-medium".into(),
        assignment_jwt: "assign-jwt".into(),
    };
    let assigned = build_request(&routed, &request_context(), Some(&assignment));
    assert_eq!(assigned.chat_model_uid, "claude-sonnet-5-medium");
    assert_eq!(assigned.model_assignment_jwt.as_deref(), Some("assign-jwt"));
}

#[test]
fn scores_the_router_on_the_latest_user_turn_alone_with_an_empty_message_id() {
    let prompt = build_devin_router_prompt(&request_context().messages).expect("router prompt");
    assert_eq!(prompt.message_id, "");
    assert_eq!(prompt.source, ChatMessageSource::User as i32);
    assert_eq!(prompt.prompt, "hello");
    assert!(build_devin_router_prompt(&[]).is_none());
}

// ---------------------------------------------------------------- stream

#[derive(Default)]
struct SeenRequests {
    seen: Mutex<Vec<(String, HeaderMap, Bytes)>>,
}

type ChatHandler = Arc<dyn Fn(&HeaderMap, Bytes) -> ChatReply + Send + Sync>;

struct ChatReply {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    keep_open: bool,
    abort: Option<AbortController>,
}

#[derive(Clone)]
struct EdgeOptions {
    user_jwt: Option<String>,
    user_jwt_status: u16,
    custom_api_server_url: Option<String>,
    assignment: Option<(String, String)>,
}

impl Default for EdgeOptions {
    fn default() -> Self {
        Self { user_jwt: None, user_jwt_status: 200, custom_api_server_url: None, assignment: None }
    }
}

struct Edge {
    base_url: String,
    seen: Arc<SeenRequests>,
    shutdown: tokio::task::JoinHandle<()>,
}

impl Drop for Edge {
    fn drop(&mut self) {
        self.shutdown.abort();
    }
}

async fn serve_edge(options: EdgeOptions, chat: ChatHandler) -> Edge {
    let seen = Arc::new(SeenRequests::default());
    let state = Arc::new((options, chat, seen.clone()));
    let router = Router::new().fallback(any(handle_edge)).with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind stub edge");
    let addr: SocketAddr = listener.local_addr().expect("stub edge addr");
    let shutdown = tokio::spawn(async move {
        let _ = axum::serve(listener, router.into_make_service()).await;
    });
    Edge { base_url: format!("http://{addr}"), seen, shutdown }
}

async fn handle_edge(
    State(state): State<Arc<(EdgeOptions, ChatHandler, Arc<SeenRequests>)>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (options, chat, seen) = state.as_ref();
    let path = uri.path().to_owned();
    seen.seen.lock().expect("seen lock").push((path.clone(), headers.clone(), body.clone()));

    if path == DEVIN_USER_JWT_PATH {
        if options.user_jwt_status != 200 {
            return (
                StatusCode::from_u16(options.user_jwt_status).expect("status"),
                [("content-type", "application/json")],
                Bytes::from_static(b"{\"code\":\"unauthenticated\"}"),
            )
                .into_response();
        }
        let response = GetUserJwtResponse {
            user_jwt: options.user_jwt.clone().unwrap_or_else(|| "jwt-1".into()),
            custom_api_server_url: options.custom_api_server_url.clone().unwrap_or_default(),
        };
        return (StatusCode::OK, [("content-type", "application/proto")], Bytes::from(response.encode_to_vec()))
            .into_response();
    }
    if path == DEVIN_ASSIGN_MODEL_PATH {
        let response = AssignModelResponse {
            assignment: options.assignment.as_ref().map(|(model_uid, assignment_jwt)| ModelAssignment {
                assignment_jwt: assignment_jwt.clone(),
                model_uid: model_uid.clone(),
                harness_uids: Vec::new(),
            }),
        };
        return (StatusCode::OK, [("content-type", "application/proto")], Bytes::from(response.encode_to_vec()))
            .into_response();
    }

    let reply = chat(&headers, body.clone());
    if let Some(controller) = reply.abort {
        controller.abort(Some(AbortReason::new("AbortError", "The operation was aborted")));
    }
    let status = StatusCode::from_u16(reply.status).expect("status");
    if reply.keep_open {
        let (mut sender, receiver) = tokio::io::duplex(64 * 1024);
        let body = reply.body;
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let _ = sender.write_all(&body).await;
            std::future::pending::<()>().await;
        });
        return Response::builder()
            .status(status)
            .header("content-type", reply.content_type)
            .body(axum::body::Body::from_stream(tokio_stream(receiver)))
            .expect("stream response");
    }
    (status, [("content-type", reply.content_type)], Bytes::from(reply.body)).into_response()
}

fn tokio_stream(
    reader: tokio::io::DuplexStream,
) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> {
    futures::stream::unfold(reader, |mut reader| async move {
        use tokio::io::AsyncReadExt;
        let mut buffer = vec![0u8; 8192];
        match reader.read(&mut buffer).await {
            Ok(0) => None,
            Ok(read) => {
                buffer.truncate(read);
                Some((Ok(Bytes::from(buffer)), reader))
            }
            Err(error) => Some((Err(error), reader)),
        }
    })
}

#[tokio::test]
async fn mints_a_user_jwt_first_then_streams_the_chat_with_the_released_cli_headers() {
    let chat: ChatHandler = Arc::new(|_headers, _body| ChatReply {
        status: 200,
        content_type: "application/connect+proto",
        body: {
            let mut body = Vec::new();
            body.extend(frame(GetChatMessageResponse {
                message_id: "m1".into(),
                delta_thinking: "weighing".into(),
                ..Default::default()
            }));
            body.extend(frame(GetChatMessageResponse {
                message_id: "m1".into(),
                delta_text: "Hel".into(),
                ..Default::default()
            }));
            body.extend(gzip_frame(GetChatMessageResponse {
                message_id: "m1".into(),
                delta_text: "lo".into(),
                ..Default::default()
            }));
            body.extend(frame(GetChatMessageResponse {
                message_id: "m1".into(),
                delta_tool_calls: vec![maho_ai::api::devin_agent::r#gen::cascade_pb::ChatToolCall {
                    id: "call-1".into(),
                    name: "read".into(),
                    arguments_json: "{\"path\":\"a.ts\"}".into(),
                    ..Default::default()
                }],
                stop_reason: StopReason::FunctionCall as i32,
                usage: Some(maho_ai::api::devin_agent::r#gen::cascade_pb::ModelUsageStats {
                    input_tokens: 11,
                    output_tokens: 7,
                    cache_read_tokens: 3,
                    cache_write_tokens: 2,
                    ..Default::default()
                }),
                ..Default::default()
            }));
            body.extend(trailer_frame());
            body
        },
        keep_open: false,
        abort: None,
    });
    let edge = serve_edge(EdgeOptions::default(), chat).await;
    let client = reqwest::Client::new();

    let mut options = StreamOptions::default();
    options.request.api_key = Some("session-abc".into());
    let events = drive(&client, &edge, &mut options).await;

    assert!(matches!(events[0], AssistantMessageEvent::Start { .. }));
    assert_eq!(deltas(&events, DeltaKind::Thinking), vec!["weighing"]);
    assert_eq!(deltas(&events, DeltaKind::Text), vec!["Hel", "lo"]);
    let Some(AssistantMessageEvent::Done { reason, message }) = events.last() else {
        panic!("expected a done event, got {:?}", events.last())
    };
    assert_eq!(*reason, maho_ai::types::DoneReason::ToolUse);
    let tool_call = message
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::ToolCall(call) => Some(call),
            _ => None,
        })
        .expect("tool call block");
    assert_eq!(tool_call.id, "call-1");
    assert_eq!(tool_call.name, "read");
    assert_eq!(tool_call.arguments.get("path"), Some(&serde_json::json!("a.ts")));
    assert_eq!(message.usage.input, 11);
    assert_eq!(message.usage.output, 7);
    assert_eq!(message.usage.cache_read, 3);
    assert_eq!(message.usage.cache_write, 2);
    assert_eq!(message.response_id.as_deref(), Some("m1"));

    let seen = edge.seen.seen.lock().expect("seen lock").clone();
    let paths: Vec<String> = seen.iter().map(|(path, _, _)| path.clone()).collect();
    assert_eq!(paths, vec![DEVIN_USER_JWT_PATH.to_owned(), DEVIN_CHAT_MESSAGE_PATH.to_owned()]);
    let (_, auth_headers, auth_body) = &seen[0];
    assert_eq!(auth_headers.get("content-type").and_then(|value| value.to_str().ok()), Some("application/proto"));
    assert_eq!(auth_headers.get("connect-protocol-version").and_then(|value| value.to_str().ok()), Some("1"));
    assert!(auth_headers.get("authorization").is_none());
    let auth_request = GetUserJwtRequest::decode(auth_body.clone()).expect("user jwt request");
    let auth_metadata = auth_request.metadata.expect("metadata");
    assert_eq!(auth_metadata.api_key, "devin-session-token$session-abc");
    assert_eq!(auth_metadata.ide_name, "devin-cli");
    assert_eq!(auth_metadata.ide_type, "chisel");
    assert_eq!(auth_metadata.user_jwt, "");

    let (_, chat_headers, chat_body) = &seen[1];
    assert_eq!(chat_headers.get("content-type").and_then(|value| value.to_str().ok()), Some("application/connect+proto"));
    assert_eq!(chat_headers.get("connect-content-encoding").and_then(|value| value.to_str().ok()), Some("gzip"));
    assert_eq!(chat_headers.get("connect-accept-encoding").and_then(|value| value.to_str().ok()), Some("gzip"));
    assert_eq!(chat_headers.get("accept-encoding").and_then(|value| value.to_str().ok()), Some("identity"));
    assert_eq!(chat_headers.get("user-agent").and_then(|value| value.to_str().ok()), Some("connect-go/1.18.1 (go1.26.3)"));
    assert!(chat_headers.get("authorization").is_none());
    let chat_request = decode_chat_frame(chat_body);
    let chat_metadata = chat_request.metadata.clone().expect("metadata");
    assert_eq!(chat_metadata.user_jwt, "jwt-1");
    assert_eq!(chat_metadata.api_key, "devin-session-token$session-abc");
    assert!(is_uuid_shape(&chat_request.cascade_id));
    assert!(is_uuid_shape(&chat_request.execution_id));
    assert_eq!(chat_request.chat_model_uid, "swe-1-6");
}

#[tokio::test]
async fn follows_the_account_host_get_user_jwt_hands_back_instead_of_the_seeded_base() {
    let tenant_chat: ChatHandler = Arc::new(|_headers, _body| ChatReply {
        status: 200,
        content_type: "application/connect+proto",
        body: {
            let mut body = frame(GetChatMessageResponse { message_id: "t1".into(), delta_text: "tenant".into(), ..Default::default() });
            body.extend(trailer_frame());
            body
        },
        keep_open: false,
        abort: None,
    });
    let tenant = serve_edge(EdgeOptions::default(), tenant_chat).await;

    let seeded_chat: ChatHandler = Arc::new(|_headers, _body| ChatReply {
        status: 500,
        content_type: "text/plain",
        body: b"wrong host".to_vec(),
        keep_open: false,
        abort: None,
    });
    let seeded = serve_edge(
        EdgeOptions { custom_api_server_url: Some(format!("{}/", tenant.base_url)), ..Default::default() },
        seeded_chat,
    )
    .await;

    let client = reqwest::Client::new();
    let mut options = StreamOptions::default();
    options.request.api_key = Some("session-abc".into());
    let events = drive(&client, &seeded, &mut options).await;

    assert!(matches!(events.last(), Some(AssistantMessageEvent::Done { .. })));
    assert_eq!(deltas(&events, DeltaKind::Text), vec!["tenant"]);
    assert_eq!(
        seeded.seen.seen.lock().expect("seen lock").iter().map(|(path, _, _)| path.clone()).collect::<Vec<_>>(),
        vec![DEVIN_USER_JWT_PATH.to_owned()]
    );
    assert_eq!(
        tenant.seen.seen.lock().expect("seen lock").iter().map(|(path, _, _)| path.clone()).collect::<Vec<_>>(),
        vec![DEVIN_CHAT_MESSAGE_PATH.to_owned()]
    );
}

#[tokio::test]
async fn fails_the_turn_before_any_chat_request_when_get_user_jwt_is_rejected_or_empty() {
    let client = reqwest::Client::new();
    let chat: ChatHandler = Arc::new(|_headers, _body| ChatReply {
        status: 200,
        content_type: "text/plain",
        body: Vec::new(),
        keep_open: false,
        abort: None,
    });

    let rejected = serve_edge(EdgeOptions { user_jwt_status: 401, ..Default::default() }, chat.clone()).await;
    let mut options = StreamOptions::default();
    options.request.api_key = Some("session-abc".into());
    let failed = drive(&client, &rejected, &mut options).await;
    let Some(AssistantMessageEvent::Error { error, .. }) = failed.last() else { panic!("expected an error event") };
    assert!(error.error_message.as_deref().unwrap_or_default().contains("401"), "{:?}", error.error_message);
    assert_eq!(
        rejected.seen.seen.lock().expect("seen lock").iter().map(|(path, _, _)| path.clone()).collect::<Vec<_>>(),
        vec![DEVIN_USER_JWT_PATH.to_owned()]
    );

    let empty = serve_edge(EdgeOptions { user_jwt: Some(String::new()), ..Default::default() }, chat).await;
    let mut options = StreamOptions::default();
    options.request.api_key = Some("session-abc".into());
    let empty_events = drive(&client, &empty, &mut options).await;
    let Some(AssistantMessageEvent::Error { error, .. }) = empty_events.last() else { panic!("expected an error event") };
    let message = error.error_message.clone().unwrap_or_default();
    assert!(message.to_lowercase().contains("user jwt"), "{message}");
    assert_eq!(
        empty.seen.seen.lock().expect("seen lock").iter().map(|(path, _, _)| path.clone()).collect::<Vec<_>>(),
        vec![DEVIN_USER_JWT_PATH.to_owned()]
    );
}

#[tokio::test]
async fn resolves_a_router_model_through_assign_model_and_chats_on_the_assigned_uid() {
    let chat: ChatHandler = Arc::new(|_headers, _body| ChatReply {
        status: 200,
        content_type: "application/connect+proto",
        body: {
            let mut body = frame(GetChatMessageResponse {
                message_id: "r1".into(),
                delta_text: "routed".into(),
                actual_model_uid: Some("claude-sonnet-5-medium".into()),
                ..Default::default()
            });
            body.extend(trailer_frame());
            body
        },
        keep_open: false,
        abort: None,
    });
    let edge = serve_edge(
        EdgeOptions { assignment: Some(("claude-sonnet-5-medium".into(), "assign-jwt".into())), ..Default::default() },
        chat,
    )
    .await;
    let mut router = model(&edge.base_url);
    router.id = "adaptive".into();
    router.compat = Some(maho_ai::model::ModelCompat(
        [("modelRouter".to_owned(), serde_json::json!(true))].into_iter().collect(),
    ));

    let mut options = StreamOptions::default();
    options.request.api_key = Some("session-abc".into());
    let events = collect(maho_ai::api::devin_agent::stream(&router, &context(), Some(options))).await;
    let Some(AssistantMessageEvent::Done { message, .. }) = events.last() else {
        panic!("expected a done event, got {:?}", events.last())
    };
    assert_eq!(message.response_model.as_deref(), Some("claude-sonnet-5-medium"));

    let seen = edge.seen.seen.lock().expect("seen lock").clone();
    assert_eq!(
        seen.iter().map(|(path, _, _)| path.clone()).collect::<Vec<_>>(),
        vec![
            DEVIN_USER_JWT_PATH.to_owned(),
            DEVIN_ASSIGN_MODEL_PATH.to_owned(),
            DEVIN_CHAT_MESSAGE_PATH.to_owned()
        ]
    );
    let (_, assign_headers, assign_body) = &seen[1];
    assert_eq!(assign_headers.get("content-type").and_then(|value| value.to_str().ok()), Some("application/proto"));
    let assign_request = AssignModelRequest::decode(assign_body.clone()).expect("assign request");
    assert_eq!(assign_request.model_router_uid, "adaptive");
    assert_eq!(assign_request.chat_message_prompt.expect("router prompt").prompt, "hi");
    assert_eq!(assign_request.metadata.expect("metadata").user_jwt, "");
    let chat_request = decode_chat_frame(&seen[2].2);
    assert_eq!(chat_request.cascade_id, assign_request.cascade_id);
    assert_eq!(chat_request.chat_model_uid, "claude-sonnet-5-medium");
    assert_eq!(chat_request.model_assignment_jwt.as_deref(), Some("assign-jwt"));
}

#[tokio::test]
async fn fails_a_router_turn_when_assign_model_returns_no_assignment() {
    let chat: ChatHandler = Arc::new(|_headers, _body| ChatReply {
        status: 200,
        content_type: "text/plain",
        body: Vec::new(),
        keep_open: false,
        abort: None,
    });
    let edge = serve_edge(EdgeOptions::default(), chat).await;
    let mut router = model(&edge.base_url);
    router.id = "adaptive".into();
    router.compat = Some(maho_ai::model::ModelCompat(
        [("modelRouter".to_owned(), serde_json::json!(true))].into_iter().collect(),
    ));

    let mut options = StreamOptions::default();
    options.request.api_key = Some("session-abc".into());
    let events = collect(maho_ai::api::devin_agent::stream(&router, &context(), Some(options))).await;
    let Some(AssistantMessageEvent::Error { error, .. }) = events.last() else { panic!("expected an error event") };
    assert!(error.error_message.as_deref().unwrap_or_default().contains("AssignModel"), "{:?}", error.error_message);
    assert_eq!(
        edge.seen.seen.lock().expect("seen lock").iter().map(|(path, _, _)| path.clone()).collect::<Vec<_>>(),
        vec![DEVIN_USER_JWT_PATH.to_owned(), DEVIN_ASSIGN_MODEL_PATH.to_owned()]
    );
}

#[tokio::test]
async fn keeps_one_tool_call_when_later_chunks_carry_the_arguments_without_the_id() {
    let chat: ChatHandler = Arc::new(|_headers, _body| ChatReply {
        status: 200,
        content_type: "application/connect+proto",
        body: {
            let mut body = frame(GetChatMessageResponse {
                message_id: "m1".into(),
                delta_tool_calls: vec![tool_call("call-1", "read", "{\"pa")],
                ..Default::default()
            });
            body.extend(frame(GetChatMessageResponse {
                message_id: "m1".into(),
                delta_tool_calls: vec![tool_call("", "", "th\":\"a.")],
                ..Default::default()
            }));
            body.extend(frame(GetChatMessageResponse {
                message_id: "m1".into(),
                delta_tool_calls: vec![tool_call("", "", "ts\"}")],
                ..Default::default()
            }));
            body.extend(frame(GetChatMessageResponse {
                message_id: "m1".into(),
                stop_reason: StopReason::FunctionCall as i32,
                ..Default::default()
            }));
            body.extend(trailer_frame());
            body
        },
        keep_open: false,
        abort: None,
    });
    let edge = serve_edge(EdgeOptions::default(), chat).await;
    let client = reqwest::Client::new();
    let mut options = StreamOptions::default();
    options.request.api_key = Some("session-abc".into());
    let events = drive(&client, &edge, &mut options).await;

    let Some(AssistantMessageEvent::Done { message, .. }) = events.last() else { panic!("expected a done event") };
    let tool_calls: Vec<&ToolCall> = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall(call) => Some(call),
            _ => None,
        })
        .collect();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].id, "call-1");
    assert_eq!(tool_calls[0].name, "read");
    assert_eq!(tool_calls[0].arguments.get("path"), Some(&serde_json::json!("a.ts")));
    assert_eq!(events.iter().filter(|event| matches!(event, AssistantMessageEvent::ToolcallStart { .. })).count(), 1);
    assert_eq!(deltas(&events, DeltaKind::ToolCall), vec!["{\"pa", "th\":\"a.", "ts\"}"]);
    assert_eq!(events.iter().filter(|event| matches!(event, AssistantMessageEvent::ToolcallEnd { .. })).count(), 1);
}

#[tokio::test]
async fn emits_only_the_new_suffix_when_a_chunk_repeats_the_accumulated_arguments() {
    let chat: ChatHandler = Arc::new(|_headers, _body| ChatReply {
        status: 200,
        content_type: "application/connect+proto",
        body: {
            let mut body = frame(GetChatMessageResponse {
                message_id: "m1".into(),
                delta_tool_calls: vec![tool_call("call-1", "read", "{\"pa")],
                ..Default::default()
            });
            body.extend(frame(GetChatMessageResponse {
                message_id: "m1".into(),
                delta_tool_calls: vec![tool_call("call-1", "read", "{\"path\":\"a.ts\"}")],
                ..Default::default()
            }));
            body.extend(trailer_frame());
            body
        },
        keep_open: false,
        abort: None,
    });
    let edge = serve_edge(EdgeOptions::default(), chat).await;
    let client = reqwest::Client::new();
    let mut options = StreamOptions::default();
    options.request.api_key = Some("session-abc".into());
    let events = drive(&client, &edge, &mut options).await;
    assert_eq!(deltas(&events, DeltaKind::ToolCall), vec!["{\"pa", "th\":\"a.ts\"}"]);
}

#[tokio::test]
async fn surfaces_a_connect_error_trailer_as_an_error_event_instead_of_an_empty_done() {
    let chat: ChatHandler = Arc::new(|_headers, _body| ChatReply {
        status: 200,
        content_type: "application/connect+proto",
        body: trailer_frame_with(
            "{\"error\":{\"code\":\"invalid_argument\",\"message\":\"an internal error occurred (trace ID: abc)\"}}",
        ),
        keep_open: false,
        abort: None,
    });
    let edge = serve_edge(EdgeOptions::default(), chat).await;
    let client = reqwest::Client::new();
    let mut options = StreamOptions::default();
    options.request.api_key = Some("session-abc".into());
    let events = drive(&client, &edge, &mut options).await;
    let Some(AssistantMessageEvent::Error { reason, error }) = events.last() else { panic!("expected an error event") };
    assert_eq!(*reason, maho_ai::types::ErrorReason::Error);
    let message = error.error_message.clone().unwrap_or_default();
    assert!(message.contains("invalid_argument"), "{message}");
    assert!(message.contains("an internal error occurred"), "{message}");
    assert!(!events.iter().any(|event| matches!(event, AssistantMessageEvent::Done { .. })));
}

#[tokio::test]
async fn maps_cascade_stop_reasons_that_carry_no_tool_call() {
    let chat: ChatHandler = Arc::new(|_headers, _body| ChatReply {
        status: 200,
        content_type: "application/connect+proto",
        body: {
            let mut body = frame(GetChatMessageResponse { message_id: "m2".into(), delta_text: "cut".into(), ..Default::default() });
            body.extend(frame(GetChatMessageResponse {
                message_id: "m2".into(),
                stop_reason: StopReason::MaxTokens as i32,
                ..Default::default()
            }));
            body.extend(trailer_frame());
            body
        },
        keep_open: false,
        abort: None,
    });
    let edge = serve_edge(EdgeOptions::default(), chat).await;
    let client = reqwest::Client::new();
    let mut options = StreamOptions::default();
    options.request.api_key = Some("session-abc".into());
    let events = drive(&client, &edge, &mut options).await;
    let Some(AssistantMessageEvent::Done { reason, .. }) = events.last() else { panic!("expected a done event") };
    assert_eq!(*reason, maho_ai::types::DoneReason::Length);
}

#[tokio::test]
async fn keeps_tool_use_from_the_cascade_stop_reason_even_when_no_tool_call_block_arrived() {
    let chat: ChatHandler = Arc::new(|_headers, _body| ChatReply {
        status: 200,
        content_type: "application/connect+proto",
        body: {
            let mut body = frame(GetChatMessageResponse { message_id: "m3".into(), delta_text: "calling".into(), ..Default::default() });
            body.extend(frame(GetChatMessageResponse {
                message_id: "m3".into(),
                stop_reason: StopReason::FunctionCall as i32,
                ..Default::default()
            }));
            body.extend(trailer_frame());
            body
        },
        keep_open: false,
        abort: None,
    });
    let edge = serve_edge(EdgeOptions::default(), chat).await;
    let client = reqwest::Client::new();
    let mut options = StreamOptions::default();
    options.request.api_key = Some("session-abc".into());
    let events = drive(&client, &edge, &mut options).await;
    let Some(AssistantMessageEvent::Done { reason, .. }) = events.last() else { panic!("expected a done event") };
    assert_eq!(*reason, maho_ai::types::DoneReason::ToolUse);
}

#[tokio::test]
async fn keeps_a_truncated_turn_as_length_even_when_a_tool_call_block_arrived() {
    let chat: ChatHandler = Arc::new(|_headers, _body| ChatReply {
        status: 200,
        content_type: "application/connect+proto",
        body: {
            let mut body = frame(GetChatMessageResponse {
                message_id: "m4".into(),
                delta_tool_calls: vec![tool_call("tc-9", "read", "{\"path\":\"a.ts\"")],
                stop_reason: StopReason::MaxTokens as i32,
                ..Default::default()
            });
            body.extend(trailer_frame());
            body
        },
        keep_open: false,
        abort: None,
    });
    let edge = serve_edge(EdgeOptions::default(), chat).await;
    let client = reqwest::Client::new();
    let mut options = StreamOptions::default();
    options.request.api_key = Some("session-abc".into());
    let events = drive(&client, &edge, &mut options).await;
    let Some(AssistantMessageEvent::Done { reason, .. }) = events.last() else { panic!("expected a done event") };
    assert_eq!(*reason, maho_ai::types::DoneReason::Length);
}

#[tokio::test]
async fn terminates_a_cascade_server_error_stop_as_an_error_event_not_a_done_event() {
    let chat: ChatHandler = Arc::new(|_headers, _body| ChatReply {
        status: 200,
        content_type: "application/connect+proto",
        body: {
            let mut body = frame(GetChatMessageResponse { message_id: "m5".into(), delta_text: "partial".into(), ..Default::default() });
            body.extend(frame(GetChatMessageResponse {
                message_id: "m5".into(),
                stop_reason: StopReason::Error as i32,
                ..Default::default()
            }));
            body.extend(trailer_frame());
            body
        },
        keep_open: false,
        abort: None,
    });
    let edge = serve_edge(EdgeOptions::default(), chat).await;
    let client = reqwest::Client::new();
    let mut options = StreamOptions::default();
    options.request.api_key = Some("session-abc".into());
    let events = drive(&client, &edge, &mut options).await;
    let Some(AssistantMessageEvent::Error { reason, error }) = events.last() else { panic!("expected an error event") };
    assert_eq!(*reason, maho_ai::types::ErrorReason::Error);
    assert!(error.error_message.is_some());
}

#[tokio::test]
async fn reports_an_http_failure_as_a_typed_error_message_instead_of_throwing() {
    let chat: ChatHandler = Arc::new(|_headers, _body| ChatReply {
        status: 403,
        content_type: "application/json",
        body: b"{\"code\":\"permission_denied\",\"message\":\"seat required\"}".to_vec(),
        keep_open: false,
        abort: None,
    });
    let edge = serve_edge(EdgeOptions::default(), chat).await;
    let client = reqwest::Client::new();
    let mut options = StreamOptions::default();
    options.request.api_key = Some("session-abc".into());
    let events = drive(&client, &edge, &mut options).await;
    let Some(AssistantMessageEvent::Error { reason, error }) = events.last() else { panic!("expected an error event") };
    assert_eq!(*reason, maho_ai::types::ErrorReason::Error);
    let message = error.error_message.clone().unwrap_or_default();
    assert!(message.contains("403") || message.contains("seat required"), "{message}");
}

#[tokio::test]
async fn ends_as_aborted_when_the_caller_aborts_mid_stream() {
    let controller = AbortController::new();
    let abort_handle = controller.clone();
    let chat: ChatHandler = Arc::new(move |_headers, _body| ChatReply {
        status: 200,
        content_type: "application/connect+proto",
        body: frame(GetChatMessageResponse { message_id: "m1".into(), delta_text: "partial".into(), ..Default::default() }),
        keep_open: true,
        abort: Some(abort_handle.clone()),
    });
    let edge = serve_edge(EdgeOptions::default(), chat).await;

    let mut options = StreamOptions::default();
    options.request.api_key = Some("session-abc".into());
    options.request.signal = Some(controller.signal());
    let events = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        collect(maho_ai::api::devin_agent::stream(&model(&edge.base_url), &context(), Some(options))),
    )
    .await
    .expect("bounded wait");

    let Some(AssistantMessageEvent::Error { reason, .. }) = events.last() else { panic!("expected an error event") };
    assert_eq!(*reason, maho_ai::types::ErrorReason::Aborted);
}

// ---------------------------------------------------------------- helpers

fn tool_call(id: &str, name: &str, arguments_json: &str) -> maho_ai::api::devin_agent::r#gen::cascade_pb::ChatToolCall {
    maho_ai::api::devin_agent::r#gen::cascade_pb::ChatToolCall {
        id: id.into(),
        name: name.into(),
        arguments_json: arguments_json.into(),
        ..Default::default()
    }
}

fn frame(message: GetChatMessageResponse) -> Vec<u8> {
    framed(0x00, &message.encode_to_vec())
}

fn gzip_frame(message: GetChatMessageResponse) -> Vec<u8> {
    framed(0x01, &gzip(&message.encode_to_vec()))
}

fn trailer_frame() -> Vec<u8> {
    trailer_frame_with("{\"metadata\":{}}")
}

fn trailer_frame_with(json: &str) -> Vec<u8> {
    framed(0x02, json.as_bytes())
}

fn framed(flags: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(flags);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn gzip(payload: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(payload).expect("gzip payload");
    encoder.finish().expect("finish gzip")
}

fn frame_of(message: GetChatMessageResponse) -> Vec<u8> {
    frame(message)
}

fn decode_chat_frame(body: &[u8]) -> GetChatMessageRequest {
    let length = u32::from_be_bytes([body[1], body[2], body[3], body[4]]) as usize;
    let payload = &body[5..5 + length];
    let bytes = if body[0] & 0x01 != 0 {
        let mut decoder = flate2::read::GzDecoder::new(payload);
        let mut out = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut out).expect("gunzip frame");
        out
    } else {
        payload.to_vec()
    };
    GetChatMessageRequest::decode(bytes.as_slice()).expect("chat request")
}

fn one_shot(bytes: Vec<u8>) -> impl futures::Stream<Item = Result<Bytes, reqwest::Error>> {
    futures::stream::once(async move { Ok(Bytes::from(bytes)) })
}

enum DeltaKind {
    Text,
    Thinking,
    ToolCall,
}

fn deltas(events: &[AssistantMessageEvent], kind: DeltaKind) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match (event, &kind) {
            (AssistantMessageEvent::TextDelta { delta, .. }, DeltaKind::Text) => Some(delta.clone()),
            (AssistantMessageEvent::ThinkingDelta { delta, .. }, DeltaKind::Thinking) => Some(delta.clone()),
            (AssistantMessageEvent::ToolcallDelta { delta, .. }, DeltaKind::ToolCall) => Some(delta.clone()),
            _ => None,
        })
        .collect()
}

async fn collect(stream: maho_ai::types::AssistantMessageEventStream) -> Vec<AssistantMessageEvent> {
    stream.collect().await.expect("stream events")
}

/// Drives one devin stream against a stub edge and returns its events.
async fn drive(client: &reqwest::Client, edge: &Edge, options: &mut StreamOptions) -> Vec<AssistantMessageEvent> {
    options.request.fetch = Some(client.clone());
    collect(maho_ai::api::devin_agent::stream(&model(&edge.base_url), &context(), Some(options.clone()))).await
}

#[allow(dead_code)]
fn unused_headers() -> HashMap<String, String> {
    HashMap::new()
}
