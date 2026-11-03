//! `runners/child-ancestry.ts`: the `spawnedBy` ancestry a child session header records.

use serde::Serialize;

use crate::runners::types::RpcRunnerSpec;

/// `NewSessionOptions["spawnedBy"]`: the child's place in the task tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnedBy {
    pub parent_session_id: String,
    pub root_session_id: String,
    pub task_id: String,
}

pub fn rpc_child_ancestry(spec: &RpcRunnerSpec) -> Option<SpawnedBy> {
    let parent = spec.parent_session_id.as_ref()?;
    Some(SpawnedBy {
        parent_session_id: parent.clone(),
        root_session_id: spec
            .root_session_id
            .clone()
            .unwrap_or_else(|| parent.clone()),
        task_id: spec.task_id.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ancestry_defaults_root_to_parent_and_is_absent_without_parent() {
        let mut spec = RpcRunnerSpec {
            task_id: "worker-task".into(),
            ..RpcRunnerSpec::default()
        };
        assert_eq!(rpc_child_ancestry(&spec), None);
        spec.parent_session_id = Some("nested-parent".into());
        assert_eq!(
            rpc_child_ancestry(&spec),
            Some(SpawnedBy {
                parent_session_id: "nested-parent".into(),
                root_session_id: "nested-parent".into(),
                task_id: "worker-task".into(),
            })
        );
        spec.root_session_id = Some("tree-root".into());
        assert_eq!(
            serde_json::to_value(rpc_child_ancestry(&spec)).expect("json"),
            serde_json::json!({"parentSessionId": "nested-parent", "rootSessionId": "tree-root", "taskId": "worker-task"})
        );
    }
}
