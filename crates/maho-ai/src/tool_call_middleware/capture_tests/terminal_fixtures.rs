//! Port of senpi packages/ai/test/tool-call-middleware/capture-terminal-fixtures.ts.

use serde_json::{Value, json};

use crate::model::Model;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, DoneReason, ErrorReason,
    ProviderNativeContent, StopReason, ThinkingContent, ToolCall,
};
use crate::utils::assistant_capture::{AssistantProgress, BlockMetadata, CapturedEvent};
use crate::utils::capture_test_records::AliasedTurn;
use crate::utils::diagnostics::AssistantMessageDiagnostic;
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::server_fallback_receipt::SERVER_FALLBACK_ABORTED_DIAGNOSTIC;

use super::downgrade::create_assistant_message;

const NATIVE_ARGUMENTS: &str = r#"{"command":"echo native"}"#;

pub(super) fn claude_model() -> Model {
    serde_json::from_value(json!({
        "id": "claude-sonnet-4-5", "name": "fixture", "api": "anthropic-messages",
        "provider": "anthropic", "baseUrl": "https://example.invalid", "reasoning": false,
        "input": ["text"], "contextWindow": 100000, "maxTokens": 4096,
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }
    }))
    .expect("claude model")
}

pub(super) fn kimi_model() -> Model {
    Model {
        id: "kimi-k2".into(),
        api: "openai-completions".into(),
        provider: "moonshot".into(),
        ..claude_model()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ending {
    Done,
    Aborted,
    Superseded,
    SupersededError,
    InvalidContentOrder,
    InvalidNativeOrder,
    Collision,
    FallbackAborted,
}

fn call(id: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "Bash".into(),
        arguments: arguments.as_object().cloned().unwrap_or_default(),
        incomplete: None,
        error_message: None,
        thought_signature: None,
        namespace: None,
    }
}

/// Signed thinking at 0, a native call at 1 and plain text at 2: every outer index equals its
/// source index.
pub(super) async fn native_call_turn(ending: Ending) -> AssistantMessageEventStream {
    let (turn, output) = native_call_body();
    end_turn(turn, output, ending).await
}

/// The same turn cut after its first `kind` event, still open: a provider still streaming.
pub(super) async fn native_call_turn_through(kind: &str) -> AssistantMessageEventStream {
    let (turn, output) = native_call_body();
    turn.finish_through(&output, kind).await.0
}

fn native_call_body() -> (AliasedTurn, AssistantMessage) {
    let mut turn = AliasedTurn::new(true);
    let mut output = create_assistant_message(json!([]), "pending");
    turn.push(AssistantMessageEvent::Start {
        partial: output.clone(),
    });
    output.content.push(ContentBlock::Thinking(ThinkingContent {
        thinking: "plan".into(),
        ..ThinkingContent::default()
    }));
    turn.push(AssistantMessageEvent::ThinkingStart {
        content_index: 0,
        partial: output.clone(),
    });
    turn.progress(AssistantProgress::metadata(BlockMetadata {
        content_index: 0,
        thinking_signature_delta: Some("sig".into()),
        ..BlockMetadata::default()
    }));
    turn.push(AssistantMessageEvent::ThinkingEnd {
        content_index: 0,
        content: "plan".into(),
        partial: output.clone(),
    });
    output
        .content
        .push(ContentBlock::ToolCall(call("native", json!({}))));
    turn.push(AssistantMessageEvent::ToolcallStart {
        content_index: 1,
        partial: output.clone(),
    });
    let native = call("native", json!({ "command": "echo native" }));
    output.content[1] = ContentBlock::ToolCall(native.clone());
    turn.push(AssistantMessageEvent::ToolcallDelta {
        content_index: 1,
        delta: NATIVE_ARGUMENTS.into(),
        partial: output.clone(),
    });
    turn.progress(AssistantProgress::MessageToolArgumentsReset {
        content_index: 1,
        raw_argument_prefix: NATIVE_ARGUMENTS.into(),
        parsed_through: NATIVE_ARGUMENTS.len(),
    });
    turn.push(AssistantMessageEvent::ToolcallEnd {
        content_index: 1,
        tool_call: native,
        partial: output.clone(),
    });
    output.content.push(ContentBlock::text(""));
    turn.push(AssistantMessageEvent::TextStart {
        content_index: 2,
        partial: output.clone(),
    });
    output.content[2] = ContentBlock::text("after");
    turn.push(AssistantMessageEvent::TextDelta {
        content_index: 2,
        delta: "after".into(),
        partial: output.clone(),
    });
    turn.push(AssistantMessageEvent::TextEnd {
        content_index: 2,
        content: "after".into(),
        partial: output.clone(),
    });
    (turn, output)
}

async fn end_turn(
    mut turn: AliasedTurn,
    mut output: AssistantMessage,
    ending: Ending,
) -> AssistantMessageEventStream {
    match ending {
        Ending::InvalidNativeOrder => {
            // A delta for a call that already ended is an invalid native order.
            turn.push(AssistantMessageEvent::ToolcallDelta {
                content_index: 1,
                delta: String::new(),
                partial: output.clone(),
            });
        }
        Ending::InvalidContentOrder => {
            // A delta for text that already ended is an invalid content order.
            turn.push(AssistantMessageEvent::TextDelta {
                content_index: 2,
                delta: String::new(),
                partial: output.clone(),
            });
        }
        Ending::Collision => {
            // A native call reusing the id the wrapper gave the call it recovered from prose
            // collides with it. Only its own partial holds it, so the id is not yet visible
            // while the wrapper recovers the prose.
            let prose =
                r#"<invoke name="Bash"><parameter name="command">echo hi</parameter></invoke>"#;
            output.content.push(ContentBlock::text(""));
            turn.push(AssistantMessageEvent::TextStart {
                content_index: 3,
                partial: output.clone(),
            });
            output.content[3] = ContentBlock::text(prose);
            turn.push(AssistantMessageEvent::TextDelta {
                content_index: 3,
                delta: prose.into(),
                partial: output.clone(),
            });
            turn.push(AssistantMessageEvent::TextEnd {
                content_index: 3,
                content: prose.into(),
                partial: output.clone(),
            });
            let mut with_twin = output.clone();
            with_twin
                .content
                .push(ContentBlock::ToolCall(call("recovered-antml-0", json!({}))));
            turn.push_literal(AssistantMessageEvent::ToolcallStart {
                content_index: 4,
                partial: with_twin,
            });
        }
        Ending::Aborted => {
            output.stop_reason = StopReason::Aborted;
            turn.push(AssistantMessageEvent::Error {
                reason: ErrorReason::Aborted,
                error: output.clone(),
            });
        }
        Ending::FallbackAborted => {
            // Its own message: the live partials the wrapper has not read yet must keep their
            // blocks.
            let mut error = output.clone();
            error.content = Vec::new();
            error.stop_reason = StopReason::Error;
            error.diagnostics = Some(vec![AssistantMessageDiagnostic {
                kind: SERVER_FALLBACK_ABORTED_DIAGNOSTIC.into(),
                timestamp: 0,
                error: None,
                details: Some(Default::default()),
            }]);
            turn.push_literal(AssistantMessageEvent::Error {
                reason: ErrorReason::Error,
                error: error.clone(),
            });
            return turn.finish(&output, Some(error)).await.0;
        }
        Ending::Done | Ending::Superseded | Ending::SupersededError => {
            // A receipt appended without an event supersedes every client tool call before it.
            if matches!(ending, Ending::Superseded | Ending::SupersededError) {
                output
                    .content
                    .push(ContentBlock::ProviderNative(ProviderNativeContent {
                        subtype: "fallback".into(),
                        raw: json!({ "type": "fallback", "from": { "model": "a" }, "to": { "model": "b" } }),
                    }));
            }
            if ending == Ending::SupersededError {
                output.stop_reason = StopReason::Error;
                turn.push(AssistantMessageEvent::Error {
                    reason: ErrorReason::Error,
                    error: output.clone(),
                });
            } else {
                output.stop_reason = StopReason::ToolUse;
                turn.push(AssistantMessageEvent::Done {
                    reason: DoneReason::ToolUse,
                    message: output.clone(),
                });
            }
        }
    }
    turn.finish(&output, Some(output.clone())).await.0
}

pub(super) fn terminal_of(events: &[CapturedEvent]) -> &CapturedEvent {
    let terminal = events.last().expect("no terminal event");
    assert!(
        matches!(
            terminal.value,
            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
        ),
        "no terminal event"
    );
    terminal
}

/// `${type}:${reason}:${errorMessage ?? ""}`.
pub(super) fn outcome(terminal: &AssistantMessageEvent) -> String {
    let value = serde_json::to_value(terminal).expect("terminal json");
    let message = crate::utils::capture_test_records::terminal_message(terminal);
    format!(
        "{}:{}:{}",
        value["type"].as_str().unwrap_or_default(),
        value["reason"].as_str().unwrap_or_default(),
        message.error_message.as_deref().unwrap_or_default()
    )
}
