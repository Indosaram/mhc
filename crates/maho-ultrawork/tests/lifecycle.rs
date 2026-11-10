//! Engine lifecycle: once-per-session injection, reminder, compaction, revision, todo reminder.

use maho_ext_api::{InputSource, ToolContent};
use maho_ultrawork::{
    CORE_CONTRACT, DIRECTIVE_MESSAGE_TYPE, Engine, InputRequest, REMINDER_MESSAGE_TYPE,
    SessionArming, Sessions, Stage, TodoResultView,
};
use serde_json::{Value, json};
use std::sync::Arc;

fn engine() -> Engine {
    Engine::new(Arc::new(Sessions::default()))
}

fn prompt(engine: &Engine, session: &str, text: &str) -> Stage {
    engine
        .on_input(
            InputRequest {
                session,
                text,
                source: InputSource::Interactive,
                streaming: false,
            },
            false,
        )
        .classification
        .stage
}

fn texts(content: &[ToolContent]) -> Vec<&str> {
    content
        .iter()
        .filter_map(|item| match item {
            ToolContent::Text { text, .. } => Some(text.as_str()),
            ToolContent::Image { .. } => None,
        })
        .collect()
}

fn init(tasks: usize) -> Value {
    let tasks: Vec<Value> = (0..tasks)
        .map(|n| json!({"content": format!("task {n}"), "status": "pending"}))
        .collect();
    json!({"op": "init", "phases": [{"name": "Work", "tasks": tasks}]})
}

fn todo(engine: &Engine, session: &str, op_details: &Value) -> bool {
    engine
        .on_tool_result(
            TodoResultView {
                session,
                tool_name: "todo",
                is_error: false,
                details: Some(op_details),
            },
            false,
        )
        .is_some()
}

#[test]
fn first_mention_injects_full_contract_exactly_once_per_session() {
    let engine = engine();
    assert_eq!(prompt(&engine, "s", "ulw fix it"), Stage::Initial);
    let first = engine.on_turn_start("s", false).expect("directive");
    assert_eq!(first.custom_type, DIRECTIVE_MESSAGE_TYPE);
    assert!(!first.display);
    assert!(texts(&first.content)[0].contains(CORE_CONTRACT));
    assert!(engine.on_turn_start("s", false).is_none());
    assert!(engine.sessions().snapshot("s").armed);
    assert!(engine.on_turn_start("other", false).is_none());
}

#[test]
fn remention_while_armed_sends_notice_not_contract() {
    let engine = engine();
    prompt(&engine, "s", "ulw one");
    engine.on_turn_start("s", false);
    assert_eq!(prompt(&engine, "s", "ultrawork two"), Stage::Reminder);
    let notice = engine.on_turn_start("s", false).expect("notice");
    assert_eq!(notice.custom_type, REMINDER_MESSAGE_TYPE);
    assert!(!texts(&notice.content)[0].contains(CORE_CONTRACT));
    assert!(texts(&notice.content)[0].len() < 600);
}

#[test]
fn prompts_without_keyword_inject_nothing() {
    let engine = engine();
    prompt(&engine, "s", "ulw one");
    engine.on_turn_start("s", false);
    assert_eq!(prompt(&engine, "s", "carry on"), Stage::None);
    assert!(engine.on_turn_start("s", false).is_none());
}

#[test]
fn preview_turn_start_leaves_pending_directive_in_place() {
    let engine = engine();
    prompt(&engine, "s", "ulw one");
    assert!(engine.on_turn_start("s", true).is_some());
    assert!(engine.on_turn_start("s", false).is_some());
    assert!(engine.on_turn_start("s", false).is_none());
}

#[test]
fn compaction_disarms_and_rearms_only_on_explicit_mention() {
    let engine = engine();
    prompt(&engine, "s", "ulw one");
    engine.on_turn_start("s", false);
    engine.on_compaction("s");
    assert_eq!(
        engine.sessions().snapshot("s"),
        SessionArming {
            armed: false,
            compaction_pending: true,
            revision: engine.sessions().snapshot("s").revision,
        }
    );
    assert_eq!(prompt(&engine, "s", "keep going"), Stage::None);
    assert!(engine.on_turn_start("s", false).is_none());
    assert_eq!(prompt(&engine, "s", "ulw again"), Stage::CompactRearm);
    let again = engine.on_turn_start("s", false).expect("directive");
    assert_eq!(again.custom_type, DIRECTIVE_MESSAGE_TYPE);
    let after = engine.sessions().snapshot("s");
    assert!(after.armed && !after.compaction_pending);
}

#[test]
fn stale_contract_revision_rearms_with_full_contract() {
    let engine = engine();
    engine.sessions().mark_armed_at("s", 1);
    assert_eq!(prompt(&engine, "s", "ulw go"), Stage::Initial);
    assert_eq!(
        engine
            .on_turn_start("s", false)
            .expect("directive")
            .custom_type,
        DIRECTIVE_MESSAGE_TYPE
    );
}

#[test]
fn disabled_flag_skips_scanning_and_state_changes() {
    let engine = engine();
    let outcome = engine.on_input(
        InputRequest {
            session: "s",
            text: "ulw go",
            source: InputSource::Interactive,
            streaming: false,
        },
        true,
    );
    assert!(!outcome.classification.effective);
    assert_eq!(engine.sessions().snapshot("s"), SessionArming::default());
    engine.sessions().mark_armed("s");
    let details = init(4);
    let view = TodoResultView {
        session: "s",
        tool_name: "todo",
        is_error: false,
        details: Some(&details),
    };
    assert!(engine.on_tool_result(view, true).is_none());
}

#[test]
fn streaming_input_gets_the_block_appended_to_its_text() {
    let engine = engine();
    let request = |text| InputRequest {
        session: "s",
        text,
        source: InputSource::Interactive,
        streaming: true,
    };
    let outcome = engine.on_input(request("ulw steer"), false);
    let text = outcome.transformed_text.expect("transformed");
    assert!(text.starts_with("ulw steer") && text.contains(CORE_CONTRACT));
    assert!(engine.on_turn_start("s", false).is_none());
    let second = engine.on_input(request("ulw again"), false);
    assert!(
        !second
            .transformed_text
            .expect("notice")
            .contains(CORE_CONTRACT)
    );
}

#[test]
fn mass_mode_attaches_inline_block_without_file_reads() {
    let engine = engine();
    prompt(&engine, "s", "mulw audit");
    let message = engine.on_turn_start("s", false).expect("directive");
    let parts = texts(&message.content);
    assert_eq!(parts.len(), 2);
    assert!((1300..=1700).contains(&parts[1].len()));
    assert!(!parts[1].contains("references/") && !parts[1].contains("SKILL.md"));
}

#[test]
fn todo_reminder_fires_on_armed_session_init_or_append() {
    let engine = engine();
    assert!(!todo(&engine, "s", &init(3)), "unarmed");
    engine.sessions().mark_armed("s");
    assert!(todo(&engine, "s", &init(2)), "armed init fires at any size");
    assert!(!todo(&engine, "s", &init(5)), "single latch");
    engine.on_compaction("s");
    let mut append = init(4);
    append["op"] = json!("append");
    assert!(todo(&engine, "s", &append), "append fires after rearm");
    engine.on_compaction("s");
    let errored = init(4);
    let view = TodoResultView {
        session: "s",
        tool_name: "todo",
        is_error: true,
        details: Some(&errored),
    };
    assert!(engine.on_tool_result(view, false).is_none(), "error result");
    let other = TodoResultView {
        session: "s",
        tool_name: "bash",
        is_error: false,
        details: Some(&errored),
    };
    assert!(engine.on_tool_result(other, false).is_none(), "other tool");
    assert!(
        todo(&engine, "s", &init(1)),
        "a single-item init also fires"
    );
    assert!(!todo(&engine, "s", &init(5)), "single latch");
}

#[test]
fn compaction_resets_the_todo_latch() {
    let engine = engine();
    engine.sessions().mark_armed("s");
    assert!(todo(&engine, "s", &init(3)));
    engine.on_compaction("s");
    assert!(todo(&engine, "s", &init(4)));
}

#[test]
fn compaction_on_a_never_armed_session_leaves_the_todo_reminder_off() {
    let engine = engine();
    engine.on_compaction("s");
    assert_eq!(engine.sessions().snapshot("s"), SessionArming::default());
    assert!(!todo(&engine, "s", &init(3)));
    assert_eq!(prompt(&engine, "s", "ulw go"), Stage::Initial);
}

#[test]
fn counts_are_computed_before_the_arming_mutation() {
    let engine = engine();
    let request = InputRequest {
        session: "s",
        text: "ulw and ultrawork",
        source: InputSource::Interactive,
        streaming: false,
    };
    let first = engine.on_input(request, false).classification;
    let second = engine.on_input(request, false).classification;
    assert_eq!(first.stage, Stage::Initial);
    assert_eq!(second.stage, Stage::Reminder);
    assert_eq!(
        (
            first.short_form_seen,
            first.long_form_seen,
            first.visible_mentions
        ),
        (true, true, 2)
    );
    assert_eq!(
        (
            second.short_form_seen,
            second.long_form_seen,
            second.visible_mentions
        ),
        (true, true, 2)
    );
}

#[test]
fn global_api_round_trips_arming_and_compaction() {
    let id = "global-api-session";
    assert_eq!(maho_ultrawork::snapshot(id), SessionArming::default());
    maho_ultrawork::mark_armed(id);
    assert!(maho_ultrawork::snapshot(id).armed);
    maho_ultrawork::mark_compact_rearm(id);
    let after = maho_ultrawork::snapshot(id);
    assert!(!after.armed && after.compaction_pending);
    maho_ultrawork::mark_armed(id);
    assert!(!maho_ultrawork::snapshot(id).compaction_pending);
}
