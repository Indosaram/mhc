//! Child identity-only daemon context (`session-context.ts`).
use crate::runners::types::RpcRunnerSpec;
use std::collections::BTreeMap;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildSessionContext {
    pub kind: &'static str,
    pub context: BTreeMap<String, String>,
}
pub fn build_child_context(spec: &RpcRunnerSpec) -> ChildSessionContext {
    let role = if spec.member_env.is_some() {
        "member"
    } else if is_dag_owned_child(spec) {
        "dag_child"
    } else {
        "child"
    };
    let mut context = BTreeMap::from([
        ("role".into(), role.into()),
        ("task_id".into(), spec.task_id.clone()),
        ("state_dir".into(), spec.state_dir.clone()),
    ]);
    if let Some(traceparent) = &spec.traceparent {
        context.insert("traceparent".into(), traceparent.clone());
    }
    if let Some(env) = &spec.member_env
        && let (Some(identity), Some(config)) = (
            env.get("SENPI_TASK_MEMBER"),
            env.get("SENPI_TASK_TEAM_CONFIG"),
        ) {
            let parts = identity.split("::").collect::<Vec<_>>();
            if let [run, name] = parts.as_slice() {
                context.insert("team_run_id".into(), (*run).into());
                context.insert("member_name".into(), (*name).into());
                context.insert("team_config".into(), config.clone());
            }
        }
    ChildSessionContext {
        kind: "worker",
        context,
    }
}

fn is_dag_owned_child(spec: &RpcRunnerSpec) -> bool {
    std::fs::read_to_string(
        std::path::Path::new(&spec.state_dir)
            .join("tasks")
            .join(format!("{}.json", spec.task_id)),
    )
    .ok()
    .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
    .is_some_and(|record| record["owner"]["kind"] == "dag")
}
pub fn resolve_child_session_path(
    state_dir: &std::path::Path,
    task_id: &str,
    now: chrono::DateTime<chrono::Utc>,
    uuid: &str,
) -> std::path::PathBuf {
    state_dir.join("sessions").join(task_id).join(format!(
        "{}_{}.jsonl",
        now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            .replace(':', "-"),
        uuid
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delegation_trace_round_trips_in_session_context() {
        let spec = RpcRunnerSpec {
            traceparent: Some("delegating-turn".into()),
            ..Default::default()
        };
        let child = build_child_context(&spec);
        assert_eq!(
            super::super::session_role::read_session_traceparent(
                &serde_json::json!({"sessionContext":child.context}),
                &BTreeMap::new()
            ),
            Some("delegating-turn".into())
        );
    }
}
