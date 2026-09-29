//! `dag/events.test.ts`: lane classification and the builders' spec-shaped wire payloads.
// allow: SIZE_OK - one test per TS builder case in events.test.ts.

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::*;
use crate::dag::types::{
    DagDiagnostic, DagNodeCounts, DagNodeError, DagNodeErrorCode, DagNodeState,
    DagNodeTransitionReason,
};

fn counts() -> DagNodeCounts {
    DagNodeCounts {
        total: 2,
        completed: 2,
        ..DagNodeCounts::default()
    }
}

fn counts_json() -> Value {
    json!({ "total": 2, "pending": 0, "blocked": 0, "scheduled": 0, "running": 0,
        "completed": 2, "failed": 0, "cancelled": 0, "skipped": 0 })
}

fn node_error() -> DagNodeError {
    DagNodeError {
        code: DagNodeErrorCode::TaskError,
        message: "boom".to_string(),
        node_id: Some("node-a".to_string()),
        at: "2026-01-01T00:00:00.000Z".to_string(),
    }
}

fn wire(payload: &DagRunEventPayload) -> Value {
    serde_json::to_value(payload).expect("payload json")
}

fn ids(values: &[&str]) -> Vec<String> {
    values.iter().map(ToString::to_string).collect()
}

fn run_flag() -> DagDiagnostic {
    DagDiagnostic::RunFlag {
        message: "heads up".to_string(),
        at: "2026-01-01T00:00:00.000Z".to_string(),
    }
}

fn every_payload() -> Vec<DagRunEventPayload> {
    vec![
        DagRunEventPayload::RunCreated {
            run_key: "k".to_string(),
            name: "n".to_string(),
            definition_fingerprint: "fp".to_string(),
            node_count: 1,
            edge_count: 0,
        },
        DagRunEventPayload::RunStarted { generation: 1 },
        DagRunEventPayload::RunPaused { reason: None },
        DagRunEventPayload::RunResumed { generation: 1 },
        DagRunEventPayload::RunCompleted { counts: counts() },
        DagRunEventPayload::RunFailed {
            error: node_error(),
            counts: counts(),
        },
        DagRunEventPayload::RunCancelled {
            reason: None,
            counts: counts(),
        },
        DagRunEventPayload::WaveStarted {
            wave_index: 0,
            node_ids: ids(&["node-a"]),
        },
        DagRunEventPayload::WaveCompleted {
            wave_index: 0,
            node_ids: ids(&["node-a"]),
        },
        DagRunEventPayload::NodeTransitioned {
            node_id: "node-a".to_string(),
            from: DagNodeState::Pending,
            to: DagNodeState::Blocked,
            reason: DagNodeTransitionReason::Unblocked,
        },
        DagRunEventPayload::NodeTaskAttached {
            node_id: "node-a".to_string(),
            task_id: "t".to_string(),
            attempt: 1,
        },
        DagRunEventPayload::NodeReused {
            node_id: "node-a".to_string(),
            task_id: "t".to_string(),
            source_run_id: "run-1".to_string(),
        },
        DagRunEventPayload::DiagnosticAdded {
            diagnostic: run_flag(),
        },
        DagRunEventPayload::StreamOverflow {
            dropped_count: 1,
            recover_after_seq: 0,
        },
    ]
}

// dagEventLane

#[test]
fn given_all_14_journaled_event_types_then_every_one_classifies_as_boundary() {
    assert_eq!(journaled_event_types().len(), 14);
    for tag in journaled_event_types() {
        assert_eq!(dag_event_lane_of(tag), Ok(DagEventLane::Boundary), "{tag}");
    }
    for payload in every_payload() {
        assert_eq!(payload_lane(&payload), DagEventLane::Boundary);
    }
}

#[test]
fn given_an_unknown_type_string_when_classified_then_it_is_rejected() {
    assert_eq!(
        dag_event_lane_of("dag.unknown.event"),
        Err(UnknownDagEventType("dag.unknown.event".to_string()))
    );
}

// dag event builders

#[test]
fn given_run_fields_when_run_created_then_spec_shaped_payload() {
    let event = DagRunEventPayload::RunCreated {
        run_key: "key-1".to_string(),
        name: "run".to_string(),
        definition_fingerprint: "fp".to_string(),
        node_count: 2,
        edge_count: 1,
    };

    assert_eq!(
        wire(&event),
        json!({ "type": "dag.run.created", "runKey": "key-1", "name": "run",
            "definitionFingerprint": "fp", "nodeCount": 2, "edgeCount": 1 })
    );
}

#[test]
fn given_a_generation_when_run_started_then_spec_shaped_payload() {
    assert_eq!(
        wire(&DagRunEventPayload::RunStarted { generation: 3 }),
        json!({ "type": "dag.run.started", "generation": 3 })
    );
}

#[test]
fn given_a_reason_when_run_paused_then_spec_shaped_payload() {
    let event = DagRunEventPayload::RunPaused {
        reason: Some("session_shutdown".to_string()),
    };

    assert_eq!(
        wire(&event),
        json!({ "type": "dag.run.paused", "reason": "session_shutdown" })
    );
}

#[test]
fn given_no_reason_when_run_paused_then_reason_omitted() {
    assert_eq!(
        wire(&DagRunEventPayload::RunPaused { reason: None }),
        json!({ "type": "dag.run.paused" })
    );
}

#[test]
fn given_a_generation_when_run_resumed_then_spec_shaped_payload() {
    assert_eq!(
        wire(&DagRunEventPayload::RunResumed { generation: 2 }),
        json!({ "type": "dag.run.resumed", "generation": 2 })
    );
}

#[test]
fn given_counts_when_run_completed_then_spec_shaped_payload() {
    assert_eq!(
        wire(&DagRunEventPayload::RunCompleted { counts: counts() }),
        json!({ "type": "dag.run.completed", "counts": counts_json() })
    );
}

#[test]
fn given_an_error_and_counts_when_run_failed_then_spec_shaped_payload() {
    let event = DagRunEventPayload::RunFailed {
        error: node_error(),
        counts: counts(),
    };

    assert_eq!(
        wire(&event),
        json!({ "type": "dag.run.failed",
            "error": { "code": "task_error", "message": "boom", "nodeId": "node-a",
                "at": "2026-01-01T00:00:00.000Z" },
            "counts": counts_json() })
    );
}

#[test]
fn given_a_reason_and_counts_when_run_cancelled_then_spec_shaped_payload() {
    let event = DagRunEventPayload::RunCancelled {
        reason: Some("user".to_string()),
        counts: counts(),
    };

    assert_eq!(
        wire(&event),
        json!({ "type": "dag.run.cancelled", "reason": "user", "counts": counts_json() })
    );
}

#[test]
fn given_a_wave_when_wave_started_then_spec_shaped_payload() {
    let event = DagRunEventPayload::WaveStarted {
        wave_index: 1,
        node_ids: ids(&["node-a", "node-b"]),
    };

    assert_eq!(
        wire(&event),
        json!({ "type": "dag.wave.started", "waveIndex": 1, "nodeIds": ["node-a", "node-b"] })
    );
}

#[test]
fn given_a_wave_when_wave_completed_then_spec_shaped_payload() {
    let event = DagRunEventPayload::WaveCompleted {
        wave_index: 0,
        node_ids: ids(&["node-a"]),
    };

    assert_eq!(
        wire(&event),
        json!({ "type": "dag.wave.completed", "waveIndex": 0, "nodeIds": ["node-a"] })
    );
}

#[test]
fn given_a_transition_when_node_transitioned_then_spec_shaped_payload() {
    let event = DagRunEventPayload::NodeTransitioned {
        node_id: "node-a".to_string(),
        from: DagNodeState::Blocked,
        to: DagNodeState::Scheduled,
        reason: DagNodeTransitionReason::Unblocked,
    };

    assert_eq!(
        wire(&event),
        json!({ "type": "dag.node.transitioned", "nodeId": "node-a", "from": "blocked",
            "to": "scheduled", "reason": { "kind": "unblocked" } })
    );
}

#[test]
fn given_a_task_attach_when_node_task_attached_then_spec_shaped_payload() {
    let event = DagRunEventPayload::NodeTaskAttached {
        node_id: "node-a".to_string(),
        task_id: "task-1".to_string(),
        attempt: 1,
    };

    assert_eq!(
        wire(&event),
        json!({ "type": "dag.node.task-attached", "nodeId": "node-a", "taskId": "task-1", "attempt": 1 })
    );
}

#[test]
fn given_a_reuse_source_when_node_reused_then_spec_shaped_payload() {
    let event = DagRunEventPayload::NodeReused {
        node_id: "node-b".to_string(),
        task_id: "task-9".to_string(),
        source_run_id: "run-1".to_string(),
    };

    assert_eq!(
        wire(&event),
        json!({ "type": "dag.node.reused", "nodeId": "node-b", "taskId": "task-9", "sourceRunId": "run-1" })
    );
}

#[test]
fn given_a_diagnostic_when_diagnostic_added_then_spec_shaped_payload() {
    let event = DagRunEventPayload::DiagnosticAdded {
        diagnostic: run_flag(),
    };

    assert_eq!(
        wire(&event),
        json!({ "type": "dag.diagnostic.added",
            "diagnostic": { "kind": "run_flag", "message": "heads up", "at": "2026-01-01T00:00:00.000Z" } })
    );
}

#[test]
fn given_an_overflow_when_stream_overflow_then_spec_shaped_payload() {
    let event = DagRunEventPayload::StreamOverflow {
        dropped_count: 5,
        recover_after_seq: 42,
    };

    assert_eq!(
        wire(&event),
        json!({ "type": "dag.stream.overflow", "droppedCount": 5, "recoverAfterSeq": 42 })
    );
}

#[test]
fn given_any_builder_output_when_inspected_then_no_envelope_seq_or_at_is_assigned_here() {
    for payload in every_payload() {
        let json = wire(&payload);
        for key in ["seq", "at", "lane", "runId"] {
            assert!(json.get(key).is_none(), "{key} present on {json}");
        }
    }
}
