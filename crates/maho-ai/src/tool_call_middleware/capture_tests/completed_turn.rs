//! Port of senpi packages/ai/test/tool-call-middleware/capture-completed-turn.test.ts.

use serde_json::json;

use crate::tool_call_middleware::context_transformer::get_protocol;
use crate::tool_call_middleware::protocols::kimi_xtml::recovery_stream::create_xtml_recovery_stream_parser;
use crate::tool_call_middleware::protocols::kimi_xtml::thinking_recovery_stream::wrap_stream_with_kimi_thinking_recovery;
use crate::tool_call_middleware::recovery_stream_wrapper::{
    InvokeRecoveryOptions, wrap_stream_with_invoke_recovery,
};
use crate::tool_call_middleware::stream_wrapper::wrap_stream_with_tool_call_middleware;
use crate::tool_call_middleware::types::ToolCallFormat;
use crate::tool_call_middleware::wrap_stream_with_model_recovery;
use crate::types::{AssistantMessageEvent, ContentBlock, DoneReason};
use crate::utils::assistant_capture::AssistantRecord;
use crate::utils::capture_test_records::{
    AliasedTurn, capture, drain_records, public_events, subscribe_capture_for_test,
    terminal_message,
};
use crate::utils::event_stream::AssistantMessageEventStream;

use super::downgrade::{bash_tool, create_assistant_message, invoke_recovery};
use super::terminal_fixtures::{Ending, claude_model, kimi_model, native_call_turn, terminal_of};

async fn text_turn(text: &str) -> AssistantMessageEventStream {
    let mut turn = AliasedTurn::new(true);
    let mut output = create_assistant_message(json!([]), "stop");
    turn.push(AssistantMessageEvent::Start {
        partial: output.clone(),
    });
    output.content.push(ContentBlock::text(""));
    turn.push(AssistantMessageEvent::TextStart {
        content_index: 0,
        partial: output.clone(),
    });
    output.content[0] = ContentBlock::text(text);
    turn.push(AssistantMessageEvent::TextDelta {
        content_index: 0,
        delta: text.into(),
        partial: output.clone(),
    });
    turn.push(AssistantMessageEvent::TextEnd {
        content_index: 0,
        content: text.into(),
        partial: output.clone(),
    });
    turn.push(AssistantMessageEvent::Done {
        reason: DoneReason::Stop,
        message: output.clone(),
    });
    turn.finish(&output, Some(output.clone())).await.0
}

const INVOKE_IN_PROSE: &str =
    r#"before <invoke name="Bash"><parameter name="command">echo hi</parameter></invoke> after"#;

#[derive(Clone, Copy, Debug)]
enum Turn {
    NativeCall,
    PlainText,
    RecoveredTextCall,
}

async fn source(turn: Turn) -> AssistantMessageEventStream {
    match turn {
        Turn::NativeCall => native_call_turn(Ending::Done).await,
        Turn::PlainText => text_turn("hello").await,
        Turn::RecoveredTextCall => text_turn(INVOKE_IN_PROSE).await,
    }
}

#[derive(Clone, Copy, Debug)]
enum Wrapper {
    InvokeRecovery,
    KimiXtmlInvokeRecovery,
    KimiThinkingRecovery,
    ModelRecoveryClaude,
    ModelRecoveryKimi,
    ToolCallMiddleware,
}

fn wrap(wrapper: Wrapper, stream: AssistantMessageEventStream) -> AssistantMessageEventStream {
    match wrapper {
        Wrapper::InvokeRecovery => invoke_recovery(stream),
        Wrapper::KimiXtmlInvokeRecovery => wrap_stream_with_invoke_recovery(
            stream,
            vec![bash_tool()],
            InvokeRecoveryOptions {
                create_parser: Some(std::sync::Arc::new(|tools| {
                    create_xtml_recovery_stream_parser(tools, None)
                })),
                protocol: ToolCallFormat::KimiXtml,
            },
        ),
        Wrapper::KimiThinkingRecovery => wrap_stream_with_kimi_thinking_recovery(stream),
        Wrapper::ModelRecoveryClaude => {
            wrap_stream_with_model_recovery(stream, &claude_model(), &[bash_tool()])
        }
        Wrapper::ModelRecoveryKimi => {
            wrap_stream_with_model_recovery(stream, &kimi_model(), &[bash_tool()])
        }
        Wrapper::ToolCallMiddleware => wrap_stream_with_tool_call_middleware(
            stream,
            get_protocol(ToolCallFormat::Antml),
            vec![bash_tool()],
        ),
    }
}

/// `it.each(cases)("$wrapper, $turn")`: every wrapper crossed with every turn.
#[tokio::test]
async fn a_completed_turn_through_a_recovery_wrapper_never_marks_a_rewrite() {
    for wrapper in [
        Wrapper::InvokeRecovery,
        Wrapper::KimiXtmlInvokeRecovery,
        Wrapper::KimiThinkingRecovery,
        Wrapper::ModelRecoveryClaude,
        Wrapper::ModelRecoveryKimi,
        Wrapper::ToolCallMiddleware,
    ] {
        for turn in [Turn::NativeCall, Turn::PlainText, Turn::RecoveredTextCall] {
            let _subscription = subscribe_capture_for_test();
            let records = drain_records(&wrap(wrapper, source(turn).await)).await;
            let events = public_events(&records);
            let terminal = terminal_of(&events);
            let case = format!("{wrapper:?}, {turn:?}");
            assert!(
                matches!(terminal.value, AssistantMessageEvent::Done { .. }),
                "{case}: not done"
            );
            assert_eq!(
                terminal_message(&terminal.value).error_message,
                None,
                "{case}"
            );
            assert!(capture(terminal).is_some(), "{case}: no terminal capture");
            let marked = records
                .iter()
                .filter(|record| match record {
                    AssistantRecord::Event(event) => {
                        capture(event).is_some_and(|capture| capture.rewritten_from.is_some())
                    }
                    AssistantRecord::Progress(_) => false,
                })
                .count();
            assert_eq!(marked, 0, "{case}: marked a rewrite");
        }
    }
}
