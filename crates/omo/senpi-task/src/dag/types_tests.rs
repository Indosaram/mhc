//! `dag/types.test.ts`. Type-level `expectTypeOf` assertions are enforced by the Rust type system;
//! the runtime assertions are translated against the serde wire shape.

use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;
use crate::tools::task::validation::TaskTargetErrorCode;

#[test]
fn given_the_journaled_payload_union_when_its_type_tags_are_enumerated_then_it_has_exactly_14_members_and_excludes_live_activity()
 {
    let mut tags = DAG_RUN_EVENT_TYPES.to_vec();

    assert_eq!(tags.len(), 14);
    assert!(!tags.contains(&"dag.node.activity"));
    tags.sort_unstable();
    let mut expected: Vec<&str> = [
        DagRunEventType::RunCreated,
        DagRunEventType::RunStarted,
        DagRunEventType::RunPaused,
        DagRunEventType::RunResumed,
        DagRunEventType::RunCompleted,
        DagRunEventType::RunFailed,
        DagRunEventType::RunCancelled,
        DagRunEventType::WaveStarted,
        DagRunEventType::WaveCompleted,
        DagRunEventType::NodeTransitioned,
        DagRunEventType::NodeTaskAttached,
        DagRunEventType::NodeReused,
        DagRunEventType::DiagnosticAdded,
        DagRunEventType::StreamOverflow,
    ]
    .into_iter()
    .map(DagRunEventType::as_str)
    .collect();
    expected.sort_unstable();
    assert_eq!(tags, expected);
}

#[test]
fn given_a_journaled_dag_event_when_constructed_then_type_and_seq_are_sibling_top_level_properties()
{
    let event = DagRunEvent {
        schema_version: SchemaVersion1,
        run_id: "run-1".to_string(),
        seq: 7,
        at: "2026-08-14T00:00:00.000Z".to_string(),
        lane: DagEventLane::Boundary,
        payload: DagRunEventPayload::RunStarted { generation: 1 },
    };

    let wire = serde_json::to_value(&event).expect("json");

    assert_eq!(
        wire,
        json!({ "schemaVersion": 1, "runId": "run-1", "seq": 7, "at": "2026-08-14T00:00:00.000Z",
            "lane": "boundary", "type": "dag.run.started", "generation": 1 })
    );
    assert!(wire.get("payload").is_none());
    assert_eq!(
        serde_json::from_value::<DagRunEvent>(wire).expect("round trip"),
        event
    );
    let mut lanes = DAG_EVENT_LANES.to_vec();
    lanes.sort_unstable();
    assert_eq!(lanes, vec!["activity", "boundary"]);
}

#[test]
fn given_live_activity_telemetry_when_inspected_then_it_is_a_separate_unsequenced_event_on_its_own_channel()
 {
    let activity = DagActivityEvent {
        schema_version: SchemaVersion1,
        run_id: "run-1".to_string(),
        node_id: "n-1".to_string(),
        task_id: "task-1".to_string(),
        at: "2026-08-14T00:00:00.000Z".to_string(),
        activity: "streaming".to_string(),
        current_tool: Some("bash".to_string()),
        last_assistant_line: Some("working".to_string()),
        turns: 3,
        tool_calls: Some(5),
    };

    let wire = serde_json::to_value(&activity).expect("json");

    assert_eq!(DAG_ACTIVITY_CHANNEL, "omo.dag.activity");
    assert!(wire.get("seq").is_none());
    assert!(wire.get("payload").is_none());
}

#[test]
fn given_a_dag_route_when_kinds_are_enumerated_then_category_xor_agent_holds_and_a_pure_model_route_is_impossible()
 {
    let category = DagRoute::Category {
        category: "quick".to_string(),
    };
    let agent = DagRoute::Agent {
        agent: "momus".to_string(),
        model: Some("openai/gpt-5".to_string()),
    };

    assert_eq!(
        serde_json::to_value(&category).expect("json"),
        json!({ "kind": "category", "category": "quick" })
    );
    assert_eq!(
        serde_json::to_value(&agent).expect("json"),
        json!({ "kind": "agent", "agent": "momus", "model": "openai/gpt-5" })
    );
    assert!(!DAG_ROUTE_KINDS.contains(&"model"));
    assert!(serde_json::from_value::<DagRoute>(json!({ "kind": "model", "model": "x" })).is_err());
}

#[test]
fn given_node_level_user_input_when_field_names_are_checked_then_they_mirror_the_task_tool_contract()
 {
    let by_category = DagNodeTarget::Category("quick".to_string());
    let by_subagent = DagNodeTarget::SubagentType {
        subagent_type: "momus".to_string(),
        model: Some("openai/gpt-5".to_string()),
    };
    let code: DagNodeTargetErrorCode = TaskTargetErrorCode::CategoryWithModel;

    assert_eq!(by_category, DagNodeTarget::Category("quick".to_string()));
    assert!(
        matches!(by_subagent, DagNodeTarget::SubagentType { ref subagent_type, .. } if subagent_type == "momus")
    );
    assert_eq!(code.as_str(), "category_with_model");
}

#[test]
fn given_the_settings_block_when_defaults_resolve_then_every_documented_default_holds() {
    assert_eq!(
        DAG_SETTINGS_DEFAULTS,
        DagSettings {
            max_nodes_per_run: 64,
            max_runs_per_session: 16,
            subscriber_ring: 1000,
            heartbeat_ms: 15000,
            history_default_limit: 256,
            history_max_limit: 1000,
            retention_days: 7,
            max_prompt_bytes: 262_144,
        }
    );
}

#[test]
fn given_the_state_vocabularies_when_enumerated_then_statuses_states_codes_and_reasons_match_the_contract()
 {
    assert_eq!(
        serde_json::to_value(DAG_RUN_STATUSES).expect("json"),
        json!([
            "pending",
            "running",
            "paused",
            "completed",
            "failed",
            "cancelled"
        ])
    );
    assert_eq!(
        serde_json::to_value(DAG_NODE_STATES).expect("json"),
        json!([
            "pending",
            "blocked",
            "scheduled",
            "running",
            "completed",
            "failed",
            "cancelled",
            "skipped"
        ])
    );
    assert_eq!(
        serde_json::to_value(DAG_NODE_ERROR_CODES).expect("json"),
        json!([
            "plan_unresolved",
            "depth_denied",
            "start_failed",
            "residency_denied",
            "task_error",
            "task_interrupted",
            "task_lost",
            "task_cancelled",
            "resume_task_missing",
            "journal_corrupt"
        ])
    );
    let kinds: Vec<serde_json::Value> = DAG_NODE_TRANSITION_REASONS
        .iter()
        .map(|reason| serde_json::to_value(reason).expect("json"))
        .collect();
    assert_eq!(
        kinds,
        [
            "unblocked",
            "scheduled",
            "started",
            "succeeded",
            "failed",
            "cancelled",
            "skipped",
            "interrupted",
            "lost",
            "resumed"
        ]
        .into_iter()
        .map(|kind| json!({ "kind": kind }))
        .collect::<Vec<_>>()
    );
    assert_eq!(
        serde_json::to_value(DagNodeTransitionReason::TaskQueued { queue_position: 3 })
            .expect("json"),
        json!({ "kind": "task_queued", "queuePosition": 3 })
    );
    for status in DAG_RUN_STATUSES {
        assert_eq!(
            serde_json::to_value(status).expect("json"),
            json!(status.as_str())
        );
    }
    for state in DAG_NODE_STATES {
        assert_eq!(
            serde_json::to_value(state).expect("json"),
            json!(state.as_str())
        );
    }
}
