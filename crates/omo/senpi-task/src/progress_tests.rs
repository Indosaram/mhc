use std::cell::Cell;
use std::rc::Rc;

use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;
use crate::state::ResolvedModelSource;

fn resolved_model() -> ResolvedModelRecord {
    ResolvedModelRecord {
        display: "Kimi K3 Unlocked".to_string(),
        reasoning_effort: Some("max".to_string()),
        ..ResolvedModelRecord::new(
            ResolvedModelSource::Category,
            "kimi-coding",
            "kimi-k3-unlocked",
        )
    }
}

fn quick_target() -> ChildProgressTarget {
    ChildProgressTarget {
        category: Some("quick".to_string()),
        resolved_model: Some(resolved_model()),
        ..ChildProgressTarget::default()
    }
}

fn tool_start(name: &str, args: serde_json::Value) -> ManagedChildEvent {
    ManagedChildEvent {
        tool_name: Some(name.to_string()),
        args: Some(args),
        ..ManagedChildEvent::of("tool_execution_start")
    }
}

fn tool_end(name: &str) -> ManagedChildEvent {
    ManagedChildEvent {
        tool_name: Some(name.to_string()),
        ..ManagedChildEvent::of("tool_execution_end")
    }
}

fn fallback(to: &str) -> ManagedChildEvent {
    ManagedChildEvent {
        to: Some(to.to_string()),
        ..ManagedChildEvent::of("retry_fallback_applied")
    }
}

#[test]
fn task_summary_leads_the_activity() {
    let target = ChildProgressTarget {
        category: Some("quick".to_string()),
        task_summary: Some("Audit the boundary".to_string()),
        description: Some("quick label".to_string()),
        ..ChildProgressTarget::default()
    };
    let progress = create_child_progress("st_00000001", target, 1_000, || 1_000);
    assert!(
        progress
            .details()
            .progress
            .activity
            .starts_with("Audit the boundary")
    );
}

#[test]
fn activity_carries_target_model_turn_and_tool_counts() {
    let now = Rc::new(Cell::new(1_000));
    let clock = {
        let now = Rc::clone(&now);
        move || now.get()
    };
    let mut progress = create_child_progress("st_00000001", quick_target(), 1_000, clock);
    progress.accept(&tool_start("read", json!({ "path": "src/foo.ts" })));
    now.set(3_000);
    progress.accept(&tool_end("read"));
    now.set(5_000);
    progress.accept(&ManagedChildEvent {
        message: Some(json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": "First line\nFinal assistant update" }],
            "usage": { "output": 100, "totalTokens": 500 },
        })),
        ..ManagedChildEvent::of("message_end")
    });
    assert_eq!(
        progress.details(),
        ToolProgressDetails {
            progress: ProgressActivity {
                activity: "st_00000001 · category:quick(kimi-coding/kimi-k3-unlocked:max) · turn 1 (1 tool) · running · 50 tok/s".to_string(),
                started_at: 1_000.0,
            },
            child_id: "st_00000001".to_string(),
            current_tool: None,
            last_assistant_line: Some("Final assistant update".to_string()),
            turns: 1.0,
            tool_calls: Some(1.0),
            tokens: Some(500.0),
            output_tokens: Some(100.0),
            tokens_per_second: Some(50.0),
        }
    );
    assert_eq!(progress.content_text(), "↳ last: Final assistant update");
}

#[test]
fn human_description_leads_instead_of_id() {
    let target = ChildProgressTarget {
        description: Some("Audit the waiting line".to_string()),
        name: Some("task-1".to_string()),
        ..quick_target()
    };
    let progress = create_child_progress("st_00000009", target, 1_000, || 2_000);
    let details = progress.details();
    assert_eq!(
        details.progress.activity,
        "Audit the waiting line · category:quick(kimi-coding/kimi-k3-unlocked:max) · turn 0 · running"
    );
    assert_eq!(details.child_id, "st_00000009");
}

#[test]
fn running_tool_is_named_and_counts_pluralize() {
    let target = ChildProgressTarget {
        agent_type: Some("momus".to_string()),
        ..ChildProgressTarget::default()
    };
    let mut progress = create_child_progress("st_00000002", target, 1_000, || 2_000);
    progress.accept(&tool_start("read", json!({ "path": "a.ts" })));
    progress.accept(&tool_end("read"));
    progress.accept(&tool_start("grep", json!({ "pattern": "TODO" })));
    progress.accept(&ManagedChildEvent {
        message: Some(
            json!({ "role": "assistant", "content": [{ "type": "text", "text": "looking" }] }),
        ),
        ..ManagedChildEvent::of("message_end")
    });
    let details = progress.details();
    assert_eq!(
        details.progress.activity,
        "st_00000002 · agent:momus · turn 1 (2 tools) · running grep TODO"
    );
    assert_eq!(details.current_tool.as_deref(), Some("grep TODO"));
    assert_eq!(details.tool_calls, Some(2.0));
}

#[test]
fn fallback_events_update_model_and_count() {
    let mut progress = create_child_progress("st_00000004", quick_target(), 1_000, || 2_000);
    progress.accept(&fallback("quotio-openai/gpt-5.6-luna-fast:high"));
    progress.accept(&fallback("anthropic-api/claude-haiku-4-5:medium"));
    assert_eq!(
        progress.details().progress.activity,
        "st_00000004 · category:quick(anthropic-api/claude-haiku-4-5:medium) · fallback:2 · turn 0 · running"
    );
}

#[test]
fn no_events_yield_base_status() {
    let target = ChildProgressTarget {
        category: Some("deep".to_string()),
        ..ChildProgressTarget::default()
    };
    let progress = create_child_progress("st_00000003", target, 1_000, || 1_000);
    assert_eq!(
        progress.details().progress.activity,
        "st_00000003 · category:deep · turn 0 · running"
    );
    assert_eq!(progress.content_text(), "");
}

#[test]
fn only_local_progress_shape_is_accepted() {
    let queued = read_tool_progress_details(
        &json!({ "progress": { "activity": "queued", "startedAt": 1 }, "childId": "st_1", "turns": 0 }),
    )
    .expect("queued shape");
    assert_eq!(
        serde_json::to_value(&queued).expect("json"),
        json!({ "progress": { "activity": "queued", "startedAt": 1.0 }, "childId": "st_1", "turns": 0.0 })
    );
    let running = read_tool_progress_details(&json!({
        "progress": { "activity": "running", "startedAt": 1 },
        "childId": "st_1", "turns": 1, "toolCalls": 2, "outputTokens": 10, "tokensPerSecond": 5,
    }))
    .expect("running shape");
    assert_eq!(
        running,
        ToolProgressDetails {
            progress: ProgressActivity {
                activity: "running".to_string(),
                started_at: 1.0,
            },
            child_id: "st_1".to_string(),
            current_tool: None,
            last_assistant_line: None,
            turns: 1.0,
            tool_calls: Some(2.0),
            tokens: None,
            output_tokens: Some(10.0),
            tokens_per_second: Some(5.0),
        }
    );
    assert_eq!(
        read_tool_progress_details(
            &json!({ "progress": { "startedAt": "1" }, "childId": "st_1", "turns": 0 })
        ),
        None
    );
    assert_eq!(
        read_tool_progress_details(&json!({
            "progress": { "activity": "x", "startedAt": 1 }, "childId": "st_1", "turns": 0, "toolCalls": "2",
        })),
        None
    );
}
