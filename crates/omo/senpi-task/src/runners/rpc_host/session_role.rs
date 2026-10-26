//! Session-scoped role and delegation context (`rpc-host/session-role.ts`).
use serde_json::Value;
use std::collections::BTreeMap;
pub const HOST_WARMUP_CONTEXT: &str = "host_warmup";
pub const SESSION_ROLES: [&str; 3] = ["child", "dag_child", "member"];
pub fn read_session_context(pi: &Value) -> Option<BTreeMap<String, String>> {
    let entries = pi
        .get("sessionContext")?
        .as_object()?
        .iter()
        .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.into())))
        .collect::<BTreeMap<_, _>>();
    (!entries.is_empty()).then_some(entries)
}
pub fn is_host_warmup_session(pi: &Value) -> bool {
    read_session_context(pi).is_some_and(|context| {
        context
            .get(HOST_WARMUP_CONTEXT)
            .is_some_and(|value| value == "1")
    })
}
fn member(env: &BTreeMap<String, String>) -> bool {
    env.contains_key("SENPI_TASK_MEMBER")
}
pub fn read_session_role(pi: &Value, env: &BTreeMap<String, String>) -> Option<String> {
    if let Some(role) = read_session_context(pi)
        .and_then(|context| context.get("role").cloned())
        .filter(|role| !role.is_empty())
    {
        return Some(if SESSION_ROLES.contains(&role.as_str()) {
            role
        } else {
            "child".into()
        });
    }
    if member(env) {
        Some("member".into())
    } else if env
        .get("OMO_SENPI_TASK_RPC_CHILD")
        .is_some_and(|value| value == "1")
    {
        Some("child".into())
    } else {
        None
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct SessionAncestry {
    pub depth: u32,
    pub root_session_id: Option<String>,
}
pub fn read_session_ancestry(
    pi: &Value,
    env: &BTreeMap<String, String>,
) -> Option<SessionAncestry> {
    let context = read_session_context(pi);
    let source = if context
        .as_ref()
        .and_then(|context| context.get("role"))
        .is_some_and(|role| !role.is_empty())
    {
        context.as_ref()?
    } else {
        read_session_role(&Value::Null, env)?;
        return Some(ancestry(
            env.get("OMO_SENPI_TASK_DEPTH"),
            env.get("OMO_SENPI_TASK_ROOT_SESSION_ID"),
        ));
    };
    Some(ancestry(source.get("depth"), source.get("root_session_id")))
}
fn ancestry(depth: Option<&String>, root: Option<&String>) -> SessionAncestry {
    SessionAncestry {
        depth: depth
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|depth| *depth >= 1)
            .unwrap_or(1),
        root_session_id: root.filter(|value| !value.is_empty()).cloned(),
    }
}
pub fn read_session_traceparent(pi: &Value, env: &BTreeMap<String, String>) -> Option<String> {
    if let Some(context) = read_session_context(pi)
        && context.get("role").is_some_and(|role| !role.is_empty()) {
            return context.get("traceparent").cloned();
        }
    read_session_role(&Value::Null, env)?;
    env.get("OMO_SENPI_TASK_TRACEPARENT").cloned()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn context_wins_and_ordinary_sessions_ignore_ambient_traceparent() {
        let mut env = BTreeMap::from([("OMO_SENPI_TASK_TRACEPARENT".into(), "stale".into())]);
        assert_eq!(read_session_traceparent(&Value::Null, &env), None);
        env.insert("OMO_SENPI_TASK_RPC_CHILD".into(), "1".into());
        assert_eq!(
            read_session_traceparent(&Value::Null, &env),
            Some("stale".into())
        );
        assert_eq!(
            read_session_traceparent(
                &serde_json::json!({"sessionContext":{"role":"child"}}),
                &env
            ),
            None
        );
        assert_eq!(
            read_session_traceparent(
                &serde_json::json!({"sessionContext":{"role":"child","traceparent":"current"}}),
                &env
            ),
            Some("current".into())
        );
    }
}
