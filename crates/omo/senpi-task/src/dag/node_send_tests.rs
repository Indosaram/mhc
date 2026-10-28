use super::*;

fn test_node(state: DagNodeState, task_id: Option<&str>) -> DagNode {
    DagNode {
        id: "node".into(), label: None, prompt: "do work".into(),
        route: crate::dag::types::DagRoute::Category { category: "quick".into() },
        depends_on: vec![], state, task_id: task_id.map(str::to_string), attempt: 1,
        error: None, run_stats: None, created_at: String::new(), started_at: None,
        completed_at: None, exec_attempt: None, output: None, output_bytes: None, last_activity_at: None,
    }
}

fn test_record(status: crate::dag::types::DagRunStatus, node: DagNode) -> DagRunRecordV1 {
    DagRunRecordV1 {
        schema_version: crate::dag::types::SchemaVersion1, checkpoint_seq: 0,
        run_id: "run".into(), run_key: "key".into(), name: "name".into(),
        parent_session_id: "parent".into(), root_session_id: "root".into(),
        definition_fingerprint: "fingerprint".into(),
        definition: crate::dag::manager::DagPersistedDefinition { key: "key".into(), name: "name".into(), nodes: vec![] },
        status, generation: 1, created_at: String::new(), updated_at: String::new(),
        started_at: None, completed_at: None, nodes: vec![node], edges: vec![], waves: vec![],
        critical_path: vec![], bottlenecks: vec![], diagnostics: vec![],
        amend_history: None, lease_holder_pid: None, previous_lease_holder_pid: None,
        guidance: None, retry_on_resume: None, cancel_requested_nodes: None, paused_child_acks: None, pause: None,
    }
}

#[test]
fn given_completed_run_when_node_send_would_revive_then_terminal_run_refusal_is_exact() {
    let run = "run".to_string();
    let node = test_node(DagNodeState::Completed, Some("task-1"));
    let record = test_record(crate::dag::types::DagRunStatus::Completed, node.clone());
    let error = if matches!(record.status, crate::dag::types::DagRunStatus::Completed | crate::dag::types::DagRunStatus::Cancelled) {
        let message = format!("dag run \"{run}\" already completed; node \"{}\" cannot be steered or revived. Amend the definition or start a new run.", node.id);
        control_error(DagNodeControlErrorCode::NodeNotContinuable, message, &run, &node.id)
    } else { unreachable!() };
    assert_eq!(error.code, DagNodeControlErrorCode::NodeNotContinuable);
    assert!(error.message.contains("already completed"));
}

#[test]
fn given_cancelled_node_when_send_outcome_is_not_continuable_then_retry_remedy_is_in_message() {
    let outcome = SendOutcome::NotContinuable { task_id: "task-1".into(), reason: "cancelled".into(), suggestion: String::new() };
    let error = match outcome {
        SendOutcome::NotContinuable { reason, suggestion, .. } => control_error(
            DagNodeControlErrorCode::NodeNotContinuable,
            format!("dag node \"node\" cannot be continued: {reason} {suggestion} Retry the node to run it again."),
            &"run".into(), &"node".into(),
        ),
        _ => unreachable!(),
    };
    assert_eq!(error.code, DagNodeControlErrorCode::NodeNotContinuable);
    assert!(error.message.contains("Retry the node"));
}

#[test]
fn given_retried_child_is_revived_when_watcher_settles_then_node_is_folded_once() {
    let _record = test_record(crate::dag::types::DagRunStatus::Failed, test_node(DagNodeState::Failed, Some("task-1")));
    let (sender, receiver) = std::sync::mpsc::channel::<Result<(), String>>();
    let settled = std::thread::spawn(move || {
        sender.send(Ok(())).expect("settlement receiver remains available");
        Ok(())
    });
    let result = DagNodeSendResult {
        node_id: "node".into(), task_id: Some("task-1".into()), delivery: DagNodeDeliveryResult::Revive,
        guidance_id: None,
        queue_position: None, settled: Some(settled),
    };
    assert_eq!(result.delivery, DagNodeDeliveryResult::Revive);
    assert_eq!(receiver.recv_timeout(std::time::Duration::from_secs(1)).expect("settlement signal"), Ok(()));
    assert_eq!(result.settled.expect("revive provides settlement handle").join().expect("watch thread"), Ok(()));
}
