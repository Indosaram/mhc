//! Port of senpi packages/ai/test/tool-call-middleware/capture-terminal-rewrite.test.ts.

use crate::api::lazy::lazy_stream;
use crate::utils::assistant_capture::CapturedEvent;
use crate::utils::assistant_capture::{
    AssistantRecord, assistant_records, carry_assistant_capture,
};
use crate::utils::capture_test_records::{
    DEADLINE, block_types, capture, carrying_records, collect_records, drain_records, event_index,
    event_type, public_events, records_on_foreign_blocks, subscribe_capture_for_test,
    terminal_message,
};

use super::downgrade::invoke_recovery;
use super::terminal_fixtures::{Ending, claude_model, native_call_turn, outcome, terminal_of};

/// The reader rule a terminal's `rewrittenFrom` sets: records for that index and above are
/// dropped.
fn kept_by_reader(
    records: &[AssistantRecord],
    rewritten_from: Option<usize>,
) -> Vec<AssistantRecord> {
    carrying_records(records)
        .into_iter()
        .filter(|record| {
            let Some(rewritten_from) = rewritten_from else {
                return true;
            };
            let index = match record {
                AssistantRecord::Progress(progress) => progress.content_index(),
                AssistantRecord::Event(event) => match event_index(&event.value) {
                    Some(public) => capture(event)
                        .and_then(|capture| capture.content_index)
                        .unwrap_or(public),
                    None => return true,
                },
            };
            index < rewritten_from
        })
        .collect()
}

struct Rewrite {
    seen: Vec<AssistantRecord>,
    terminal: CapturedEvent,
}

async fn ended(ending: Ending) -> Rewrite {
    let records = drain_records(&invoke_recovery(native_call_turn(ending).await)).await;
    let terminal = terminal_of(&public_events(&records)).clone();
    Rewrite {
        seen: records,
        terminal,
    }
}

/// The consumer stops after the text starts; the cancellation terminal is read afterwards. The
/// provider is still streaming when it stops (TS reads the buffered turn one record per turn, so
/// the wrapper never finishes it first; Rust's relay would, so the source stays open past the
/// text start instead).
async fn cancelled_after_text_start() -> Rewrite {
    let source = native_call_turn_through_text_start().await;
    let wrapped = invoke_recovery(source);
    let reader = assistant_records(&wrapped);
    let mut seen = Vec::new();
    loop {
        let record = tokio::time::timeout(DEADLINE, reader.next())
            .await
            .expect("record deadline")
            .expect("record")
            .expect("record before text_start");
        let stop = matches!(&record, AssistantRecord::Event(event) if event_type(&event.value) == "text_start");
        seen.push(record);
        if stop {
            break;
        }
    }
    tokio::time::timeout(DEADLINE, wrapped.cancel())
        .await
        .expect("cancel deadline")
        .expect("cancel");
    let rest = collect_records(&wrapped).await;
    Rewrite {
        seen,
        terminal: terminal_of(&public_events(&rest)).clone(),
    }
}

/// `nativeCallTurn("done")` cut after its text start, still open.
async fn native_call_turn_through_text_start()
-> crate::utils::event_stream::AssistantMessageEventStream {
    super::terminal_fixtures::native_call_turn_through("text_start").await
}

struct Row {
    kind: &'static str,
    outcome: &'static str,
    from: usize,
}

const TOOL_REMOVING_TERMINALS: [Row; 8] = [
    Row {
        kind: "aborted source",
        outcome: "error:aborted:Request was aborted",
        from: 1,
    },
    Row {
        kind: "cancelled consumer",
        outcome: "error:error:Assistant message stream consumption was cancelled",
        from: 1,
    },
    Row {
        kind: "superseded by a fallback receipt",
        outcome: "done:toolUse:",
        from: 1,
    },
    Row {
        kind: "superseded by a fallback receipt on a source error",
        outcome: "error:error:",
        from: 1,
    },
    Row {
        kind: "invalid content event order",
        outcome: "error:error:Invalid assistant content event order",
        from: 1,
    },
    Row {
        kind: "invalid native event order",
        outcome: "error:error:Invalid native tool call event order",
        from: 1,
    },
    Row {
        kind: "tool call id collision",
        outcome: "error:error:Tool call ID collision in provider stream",
        from: 1,
    },
    Row {
        kind: "forwarded fallback abort",
        outcome: "error:error:",
        from: 0,
    },
];

async fn run(kind: &str) -> Rewrite {
    match kind {
        "aborted source" => ended(Ending::Aborted).await,
        "cancelled consumer" => cancelled_after_text_start().await,
        "superseded by a fallback receipt" => ended(Ending::Superseded).await,
        "superseded by a fallback receipt on a source error" => {
            ended(Ending::SupersededError).await
        }
        "invalid content event order" => ended(Ending::InvalidContentOrder).await,
        "invalid native event order" => ended(Ending::InvalidNativeOrder).await,
        "tool call id collision" => ended(Ending::Collision).await,
        "forwarded fallback abort" => ended(Ending::FallbackAborted).await,
        other => panic!("unknown row {other}"),
    }
}

// recovery terminals that rewrite content mark the records they invalidate #4272

#[tokio::test]
async fn marks_nothing_when_the_terminal_keeps_every_block_at_its_index() {
    let _subscription = subscribe_capture_for_test();
    let Rewrite { seen, terminal } = ended(Ending::Done).await;
    let content = terminal_message(&terminal.value);
    assert_eq!(block_types(content), ["thinking", "toolCall", "text"]);
    assert_eq!(
        capture(&terminal).and_then(|capture| capture.rewritten_from),
        None
    );
    assert_eq!(
        records_on_foreign_blocks(&carrying_records(&seen), content),
        Vec::<String>::new()
    );
}

/// `it.each(toolRemovingTerminals)("a $kind terminal marks rewrittenFrom $from")`.
#[tokio::test]
async fn a_tool_removing_terminal_marks_rewritten_from() {
    for row in &TOOL_REMOVING_TERMINALS {
        let _subscription = subscribe_capture_for_test();
        let Rewrite { seen, terminal } = run(row.kind).await;
        let content = terminal_message(&terminal.value);
        assert_eq!(outcome(&terminal.value), row.outcome, "{}", row.kind);
        assert!(
            !block_types(content).contains(&"toolCall"),
            "{}: tool call kept",
            row.kind
        );
        assert_ne!(
            records_on_foreign_blocks(&carrying_records(&seen), content),
            Vec::<String>::new(),
            "{}",
            row.kind
        );
        assert_eq!(
            capture(&terminal).and_then(|capture| capture.rewritten_from),
            Some(row.from),
            "{}",
            row.kind
        );
        assert_eq!(
            records_on_foreign_blocks(&kept_by_reader(&seen, Some(row.from)), content),
            Vec::<String>::new(),
            "{}",
            row.kind
        );
    }
}

#[tokio::test]
async fn keeps_the_mark_when_a_later_wrapper_or_the_agent_re_carries_the_terminal_onto_its_own_message()
 {
    let _subscription = subscribe_capture_for_test();
    let Rewrite { terminal, .. } = ended(Ending::Aborted).await;
    let message = terminal_message(&terminal.value).clone();
    // TS carries onto `{ type: "message_end", message }`; the carried value is the message.
    let carried = carry_assistant_capture(&terminal, message.clone(), Some(&message));
    assert_eq!(
        carried
            .capture
            .as_ref()
            .and_then(|capture| capture.rewritten_from),
        Some(1)
    );
}

#[tokio::test]
async fn keeps_the_mark_across_the_lazy_stream_forwarder() {
    let _subscription = subscribe_capture_for_test();
    let source = native_call_turn(Ending::Aborted).await;
    let lazy = lazy_stream(&claude_model(), async move { Ok(invoke_recovery(source)) });
    let terminal = terminal_of(&public_events(&drain_records(&lazy).await)).clone();
    assert_eq!(
        capture(&terminal).and_then(|capture| capture.rewritten_from),
        Some(1)
    );
}
