use super::*;

#[test]
fn given_completed_node_when_retry_requested_then_node_not_retryable_is_returned() {
    let mut record = retry_record();
    record.nodes[0].state = DagNodeState::Completed;
    let error = assert_retryable(&record, &record.run_id, &["broken".into()]).unwrap_err();
    assert_eq!(error.code, DagNodeControlErrorCode::NodeNotRetryable);
    assert_eq!(error.node_ids, vec!["broken"]);
    assert!(error.message.contains("amend the definition"));
}

#[test]
fn given_retry_prompt_without_one_explicit_node_then_invalid_arguments_is_returned() {
    let options = Some(DagRetryOptions { prompt: Some("replacement".into()) });
    let node_ids: Option<&[DagNodeId]> = None;
    assert!(options.as_ref().and_then(|options| options.prompt.as_ref()).is_some());
    assert!(node_ids.is_none_or(|ids| ids.len() != 1));
    let error = control_error(DagNodeControlErrorCode::InvalidArguments,
        "a retry prompt override requires exactly one explicit node id".into(), &"run".into(), vec![]);
    assert_eq!(error.code, DagNodeControlErrorCode::InvalidArguments);
    assert_eq!(error.message, "a retry prompt override requires exactly one explicit node id");
}

#[test]
fn given_skipped_descendant_chain_when_ancestor_is_selected_then_dependents_restore_transitively() {
    let input = crate::dag::graph::DagDefinition {
        key: "test".into(),
        name: "test".into(),
        nodes: vec![crate::dag::graph::DagNodeInput {
            id: "broken".into(), prompt: "do work".into(),
            target: crate::dag::types::DagNodeTarget::Category("quick".into()),
            label: None, depends_on: None, task_summary: None, description: None, load_skills: None,
        }],
    };
    let compiled = crate::dag::graph::compile_dag(&input, &crate::dag::graph::DagCompileOptions { at: None, settings: None });
    let mut record = DagRunRecordV1 {
        schema_version: crate::dag::types::SchemaVersion1,
        checkpoint_seq: 0,
        run_id: "run".into(), run_key: "test".into(), name: "test".into(),
        parent_session_id: "parent".into(), root_session_id: "root".into(),
        definition_fingerprint: "fingerprint".into(),
        definition: crate::dag::manager::DagPersistedDefinition { key: "test".into(), name: "test".into(), nodes: vec![] },
        status: DagRunStatus::Failed, generation: 1,
        created_at: String::new(), updated_at: String::new(), started_at: None, completed_at: None,
        nodes: compiled.nodes, edges: compiled.edges, waves: compiled.waves,
        critical_path: compiled.critical_path, bottlenecks: compiled.bottlenecks, diagnostics: compiled.diagnostics,
        amend_history: None, lease_holder_pid: None, previous_lease_holder_pid: None,
        guidance: None, retry_on_resume: None, cancel_requested_nodes: None, paused_child_acks: None, pause: None,
    };
    let root = "broken".to_string();
    let dependent = DagNode {
        id: "dependent".into(),
        state: DagNodeState::Skipped,
        depends_on: vec![root.clone()],
        ..record.nodes[0].clone()
    };
    let grandchild = DagNode {
        id: "grandchild".into(),
        state: DagNodeState::Skipped,
        depends_on: vec!["dependent".into()],
        ..record.nodes[0].clone()
    };
    record.nodes = vec![record.nodes[0].clone(), dependent, grandchild];

    let restored = skipped_dependents(&record, &[root]);
    assert_eq!(restored.iter().map(|node| node.id.as_str()).collect::<Vec<_>>(), vec!["dependent", "grandchild"]);
}

fn retry_record() -> DagRunRecordV1 {
    let input = crate::dag::graph::DagDefinition {
        key: "test".into(), name: "test".into(),
        nodes: vec![crate::dag::graph::DagNodeInput {
            id: "broken".into(), prompt: "do work".into(),
            target: crate::dag::types::DagNodeTarget::Category("quick".into()),
            label: None, depends_on: None, task_summary: None, description: None, load_skills: None,
        }],
    };
    let compiled = crate::dag::graph::compile_dag(&input, &crate::dag::graph::DagCompileOptions { at: None, settings: None });
    DagRunRecordV1 {
        schema_version: crate::dag::types::SchemaVersion1, checkpoint_seq: 0,
        run_id: "run".into(), run_key: "test".into(), name: "test".into(),
        parent_session_id: "parent".into(), root_session_id: "root".into(),
        definition_fingerprint: "fingerprint".into(),
        definition: crate::dag::manager::DagPersistedDefinition { key: "test".into(), name: "test".into(), nodes: vec![] },
        status: DagRunStatus::Failed, generation: 1,
        created_at: String::new(), updated_at: String::new(), started_at: None, completed_at: None,
        nodes: compiled.nodes, edges: compiled.edges, waves: compiled.waves,
        critical_path: compiled.critical_path, bottlenecks: compiled.bottlenecks, diagnostics: compiled.diagnostics,
        amend_history: None, lease_holder_pid: None, previous_lease_holder_pid: None,
        guidance: None, retry_on_resume: None, cancel_requested_nodes: None, paused_child_acks: None, pause: None,
    }
}
