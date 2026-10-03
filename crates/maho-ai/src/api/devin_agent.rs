//! Port of senpi packages/ai/src/api/devin-agent.ts.
//!
//! Devin (Cognition/Codeium Cascade) agent API. One assistant turn is up to three RPCs, exactly as
//! the released Devin CLI performs them: `GetUserJwt` mints the short-lived user JWT the chat call
//! must carry and names the account's API host; `AssignModel` resolves a server-side router into a
//! concrete model uid plus the JWT that authorizes it; `GetChatMessage` streams the turn. Cascade
//! reports text, thinking and tool calls as separate delta fields on the same message, so this
//! adapter owns the block bookkeeping senpi's event protocol expects.

pub mod discovery;
pub mod frames;
pub mod r#gen;
pub mod metadata;
pub mod paths;
pub mod request;
pub mod stream_state;
pub mod trailer;
pub mod types;
pub mod unary;
pub mod wire;

use crate::api::devin_agent::frames::DevinFrameError;
use crate::api::devin_agent::r#gen::cascade_pb::{
    AssignModelRequest, AssignModelResponse, GetChatMessageResponse, GetUserJwtRequest,
    GetUserJwtResponse,
};
use crate::api::devin_agent::request::{build_devin_chat_request, build_devin_router_prompt, DevinChatRequestInput, DevinModelAssignment};
use crate::api::devin_agent::stream_state::{apply_devin_response, create_devin_stream_state, finalize_blocks, DevinStreamState};
use crate::api::devin_agent::types::DevinAgentOptions;
use crate::api::devin_agent::unary::{post_devin_unary, DevinUnaryInput};
use crate::api::devin_agent::wire::{
    decode_devin_frames, encode_devin_request_frame, read_devin_trailer_error, DevinFrame,
    DEVIN_ASSIGN_MODEL_PATH,
    DEVIN_CHAT_HEADERS, DEVIN_CHAT_MESSAGE_PATH, DEVIN_DEFAULT_BASE_URL, DEVIN_USER_JWT_PATH,
};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Context, DoneReason, ErrorReason, Model, ProviderResponse,
    SimpleStreamOptions, StopReason, StreamOptions, Usage,
};
use crate::utils::abort::{race_with_abort_signal, AbortSignal};
use crate::utils::event_stream::{create_assistant_message_event_stream, AssistantMessageEventStream};
use crate::utils::headers::headers_to_record;

/// `stream`.
pub fn stream(model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let model = model.clone();
    let context = context.clone();
    let options = options.unwrap_or_default();
    let sink = stream.clone();
    tokio::spawn(async move {
        run(&model, &context, &sink, &options).await;
    });
    stream
}

/// `streamSimple`.
pub fn stream_simple(model: &Model, context: &Context, options: Option<SimpleStreamOptions>) -> AssistantMessageEventStream {
    stream(model, context, options.map(|simple| simple.stream))
}

struct DevinSession {
    user_jwt: String,
    /// Host the account is provisioned on; GetUserJwt may move it off the seed.
    chat_base_url: String,
}

fn create_output(model: &Model) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        stop_details: None,
        deferred: None,
        error_message: None,
        abort_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: crate::utils::diagnostics::now_ms(),
    }
}

async fn run(
    model: &Model,
    context: &Context,
    events: &AssistantMessageEventStream,
    options: &StreamOptions,
) {
    let mut output = create_output(model);
    let mut state = create_devin_stream_state();
    let signal = options.request.signal.clone();
    let devin = DevinAgentOptions::from_stream_options(options);

    events.push(AssistantMessageEvent::Start { partial: output.clone() });
    match drive(model, context, options, &devin, &mut output, &mut state, events).await {
        Ok(()) => {}
        Err(error) => {
            finalize_blocks(&mut output, events, &mut state);
            let aborted = signal.as_ref().is_some_and(AbortSignal::aborted);
            output.stop_reason = if aborted { StopReason::Aborted } else { StopReason::Error };
            output.error_message = Some(if aborted { String::from("Request was aborted") } else { error });
            events.push(AssistantMessageEvent::Error {
                reason: if aborted { ErrorReason::Aborted } else { ErrorReason::Error },
                error: output.clone(),
            });
            events.end(None);
        }
    }
}

async fn drive(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    devin: &DevinAgentOptions,
    output: &mut AssistantMessage,
    state: &mut DevinStreamState,
    events: &AssistantMessageEventStream,
) -> Result<(), String> {
    let signal = options.request.signal.clone();
    let base_url = {
        let raw = if model.base_url.trim().is_empty() { DEVIN_DEFAULT_BASE_URL } else { &model.base_url };
        raw.trim_end_matches('/').to_owned()
    };
    let cascade_id = devin.cascade_id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let client = options.request.fetch.clone().unwrap_or_default();
    let session = mint_user_jwt(&client, &base_url, devin.api_key.as_deref(), signal.as_ref()).await?;
    let assignment = if model.compat.as_ref().and_then(|compat| compat.devin_agent().model_router) == Some(true) {
        Some(
            assign_model(model, context, &client, &session.chat_base_url, &cascade_id, devin.api_key.as_deref(), signal.as_ref())
                .await?,
        )
    } else {
        None
    };
    if let Some(assignment) = assignment.as_ref() {
        output.response_model = Some(assignment.model_uid.clone());
    }

    let request = build_devin_chat_request(&DevinChatRequestInput {
        model,
        context,
        api_key: devin.api_key.as_deref(),
        user_jwt: Some(session.user_jwt.as_str()),
        cascade_id: &cascade_id,
        assignment: assignment.as_ref(),
        max_tokens: options.max_tokens,
        temperature: options.temperature,
        top_p: None,
        stop_sequences: None,
    });
    let mut http_request = client.post(format!("{}{}", session.chat_base_url, DEVIN_CHAT_MESSAGE_PATH));
    for (name, value) in DEVIN_CHAT_HEADERS {
        http_request = http_request.header(name, value);
    }
    let send = http_request.body(encode_devin_request_frame(&request)).send();
    let response = match signal.as_ref() {
        Some(signal) => race_with_abort_signal(send, signal)
            .await
            .map_err(|_| String::from("Request was aborted"))?,
        None => send.await,
    }
    .map_err(|error| error.to_string())?;

    options.request.apply_response_hook(
            &ProviderResponse { status: response.status().as_u16(), headers: headers_to_record(response.headers()) },
            model,
        ).await?;

    let status = response.status();
    if !status.is_success() {
        let detail = response.text().await.unwrap_or_default();
        return Err(format!("Devin request failed (HTTP {}){}", status.as_u16(), detail_of(&detail)));
    }

    decode_devin_frames::<GetChatMessageResponse, _, _>(response.bytes_stream(), |frame| {
        match frame {
            DevinFrame::Message(message) => apply_devin_response(&message, output, events, state),
            // senpi throws out of the frame loop on a rejection trailer, so the turn terminates as
            // an error instead of settling into a done event; the decode is aborted here for the
            // same reason.
            DevinFrame::Trailer(trailer) => {
                if let Some(error) = read_devin_trailer_error(&trailer) {
                    return Err(DevinFrameError::Rejected(error.formatted));
                }
            }
        }
        Ok(())
    })
    .await
    .map_err(|error| error.to_string())?;

    finalize_blocks(output, events, state);
    // Cascade can close a turn that carries a tool call without ever sending an explicit stop
    // reason. Only the default "stop" is upgraded: a server-reported "length" means the turn was
    // truncated, and a truncated tool call must not be advertised as a complete one.
    if output.stop_reason == StopReason::Stop
        && output.content.iter().any(|block| matches!(block, ContentBlock::ToolCall(_)))
    {
        output.stop_reason = StopReason::ToolUse;
    }
    // Cascade reports a server error or a content filter as a stop reason on an otherwise
    // well-formed stream. senpi's protocol has no "done because it failed", so that turn terminates
    // as an error event, not a done event.
    if output.stop_reason == StopReason::Error {
        if output.error_message.is_none() {
            output.error_message = Some(String::from("Devin ended the turn with a server error or a content filter"));
        }
        events.push(AssistantMessageEvent::Error { reason: ErrorReason::Error, error: output.clone() });
        events.end(None);
        return Ok(());
    }
    events.push(AssistantMessageEvent::Done { reason: done_reason_of(output.stop_reason), message: output.clone() });
    events.end(None);
    Ok(())
}

async fn mint_user_jwt(
    client: &reqwest::Client,
    base_url: &str,
    api_key: Option<&str>,
    signal: Option<&AbortSignal>,
) -> Result<DevinSession, String> {
    let request = GetUserJwtRequest { metadata: Some(crate::api::devin_agent::metadata::devin_cli_metadata(api_key, "")) };
    let response: GetUserJwtResponse = post_devin_unary(DevinUnaryInput {
        client,
        base_url,
        path: DEVIN_USER_JWT_PATH,
        request: &request,
        signal,
    })
    .await
    .map_err(|error| error.to_string())?;
    if response.user_jwt.is_empty() {
        return Err(String::from("Devin GetUserJwt returned an empty user JWT"));
    }
    let custom_host = response.custom_api_server_url.trim_end_matches('/').to_owned();
    let chat_base_url = if custom_host.is_empty() { base_url.to_owned() } else { custom_host };
    Ok(DevinSession { user_jwt: response.user_jwt, chat_base_url })
}

/// A router uid is never a legal chat model uid, so a failed assignment fails the turn instead of
/// falling back to sending the router id to GetChatMessage.
async fn assign_model(
    model: &Model,
    context: &Context,
    client: &reqwest::Client,
    base_url: &str,
    cascade_id: &str,
    api_key: Option<&str>,
    signal: Option<&AbortSignal>,
) -> Result<DevinModelAssignment, String> {
    let request = AssignModelRequest {
        metadata: Some(crate::api::devin_agent::metadata::devin_cli_metadata(api_key, "")),
        model_router_uid: model.upstream_model_id.clone().unwrap_or_else(|| model.id.clone()),
        cascade_id: cascade_id.to_owned(),
        chat_message_prompt: build_devin_router_prompt(&context.messages),
    };
    let response: AssignModelResponse = post_devin_unary(DevinUnaryInput {
        client,
        base_url,
        path: DEVIN_ASSIGN_MODEL_PATH,
        request: &request,
        signal,
    })
    .await
    .map_err(|error| error.to_string())?;
    let assignment = response.assignment.unwrap_or_default();
    if assignment.model_uid.is_empty() || assignment.assignment_jwt.is_empty() {
        return Err(String::from("Devin AssignModel returned no model uid and assignment JWT"));
    }
    Ok(DevinModelAssignment { model_uid: assignment.model_uid, assignment_jwt: assignment.assignment_jwt })
}

/// Narrows a settled stop reason to the three senpi accepts on a done event.
fn done_reason_of(stop_reason: StopReason) -> DoneReason {
    match stop_reason {
        StopReason::Length => DoneReason::Length,
        StopReason::ToolUse => DoneReason::ToolUse,
        _ => DoneReason::Stop,
    }
}

/// `detail(response)`: the first 500 characters of a failed response body, prefixed when present.
fn detail_of(body: &str) -> String {
    if body.is_empty() {
        String::new()
    } else {
        let sliced: String = body.chars().take(500).collect();
        format!(": {sliced}")
    }
}
