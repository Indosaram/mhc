//! Port of senpi `terminal/rpc-monitor-targets.ts`: what an rpc monitor watches and the
//! `subscribe_sessions` frame it sends for it.
use serde_json::{Map, Value, json};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RpcTarget {
    Children { parent: String },
    All,
    Id { id: String },
    Task { task_id: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RpcEvents {
    All,
    Kinds(Vec<String>),
}

/// Upstream `endpointSockets`: every endpoint socket the user runs under this agent dir (hosts and
/// terminal endpoints); a directory that names no socket is skipped.
pub fn endpoint_sockets(agent_dir: &std::path::Path) -> Vec<String> {
    maho_rpc::host_endpoints::list_host_endpoints(agent_dir)
        .into_iter()
        .filter_map(|endpoint| endpoint.socket)
        .collect()
}

const STATUS_KINDS: [&str; 2] = ["session_status", "effect_status"];

pub fn parse_rpc_target(target: &str, self_durable_id: Option<&str>) -> Result<RpcTarget, String> {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return Err(
            "target is empty; pass \"children\", \"all\" or a session id, path or st_ task id"
                .to_owned(),
        );
    }
    if trimmed == "children" {
        return match self_durable_id {
            None => Err("target \"children\" needs a session with a durable id; save the session first, or watch \"all\"".to_owned()),
            Some(parent) => Ok(RpcTarget::Children { parent: parent.to_owned() }),
        };
    }
    if trimmed == "all" {
        return Ok(RpcTarget::All);
    }
    if trimmed.starts_with("st_") {
        return Ok(RpcTarget::Task {
            task_id: trimmed.to_owned(),
        });
    }
    Ok(RpcTarget::Id {
        id: trimmed.to_owned(),
    })
}

/// `["session_status", "effect_status"]` for children, `["session_status"]` otherwise.
pub fn default_events(target: &RpcTarget) -> Vec<String> {
    match target {
        RpcTarget::Children { .. } => vec!["session_status".into(), "effect_status".into()],
        _ => vec!["session_status".into()],
    }
}

/// The `subscribe_sessions` frame (without id/type) for one endpoint.
pub fn subscription_for(target: &RpcTarget, events: &RpcEvents) -> Value {
    let sessions = match target {
        RpcTarget::Children { parent } => json!({"childrenOf": parent}),
        RpcTarget::Id { id } => json!({"ids": [id]}),
        _ => json!("all"),
    };
    let mirror = match events {
        RpcEvents::All => Some(json!("all")),
        RpcEvents::Kinds(kinds) => {
            let content: Vec<&String> = kinds
                .iter()
                .filter(|kind| !STATUS_KINDS.contains(&kind.as_str()))
                .collect();
            (!content.is_empty()).then(|| json!(content))
        }
    };
    let mut frame = Map::new();
    frame.insert("sessions".into(), sessions);
    frame.insert("include_workers".into(), json!(true));
    frame.insert(
        "include_context".into(),
        json!(matches!(
            target,
            RpcTarget::Children { .. } | RpcTarget::Task { .. }
        )),
    );
    frame.insert(
        "effects".into(),
        json!(match events {
            RpcEvents::All => true,
            RpcEvents::Kinds(kinds) => kinds.iter().any(|kind| kind == "effect_status"),
        }),
    );
    if let Some(mirror) = mirror {
        frame.insert("mirror".into(), mirror);
    }
    Value::Object(frame)
}

/// Whether a delivered record is one the monitor reports.
pub fn wants_record(events: &RpcEvents, exclude: &[String], kind: &str) -> bool {
    if exclude.iter().any(|excluded| excluded == kind) {
        return false;
    }
    match events {
        RpcEvents::All => true,
        RpcEvents::Kinds(kinds) => kinds.iter().any(|wanted| wanted == kind),
    }
}

/// For a `st_` target: whether a record or snapshot row belongs to that task.
pub fn matches_task(target: &RpcTarget, row: &Value) -> bool {
    let RpcTarget::Task { task_id } = target else {
        return true;
    };
    row.get("context")
        .and_then(|context| context.get("task_id"))
        .and_then(Value::as_str)
        == Some(task_id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// rpc-monitor-targets.ts `endpointSockets`: every endpoint directory that names a socket is
    /// listed; one whose record names no socket (a foreign record) is skipped.
    #[test]
    fn endpoint_sockets_lists_named_sockets_and_skips_unnamed_endpoints() {
        let temp = tempfile::tempdir().unwrap();
        assert!(endpoint_sockets(temp.path()).is_empty());
        for (socket, recorded) in [
            ("/run/a.sock", "/run/a.sock"),
            ("/run/b.sock", "/run/other.sock"),
        ] {
            let paths = maho_rpc::host_daemon_paths::create_host_daemon_paths(socket, temp.path());
            maho_rpc::host_daemon_paths::create_daemon_directories(&paths).unwrap();
            std::fs::write(
                &paths.endpoint_file,
                json!({"socket": recorded}).to_string(),
            )
            .unwrap();
        }
        assert_eq!(
            endpoint_sockets(temp.path()),
            vec!["/run/a.sock".to_owned()]
        );
    }

    #[test]
    fn parses_targets() {
        assert!(parse_rpc_target("  ", None).is_err());
        assert!(parse_rpc_target("children", None).is_err());
        assert_eq!(
            parse_rpc_target("children", Some("d1")),
            Ok(RpcTarget::Children {
                parent: "d1".into()
            })
        );
        assert_eq!(parse_rpc_target(" all ", None), Ok(RpcTarget::All));
        assert_eq!(
            parse_rpc_target("st_1", None),
            Ok(RpcTarget::Task {
                task_id: "st_1".into()
            })
        );
        assert_eq!(
            parse_rpc_target("abc", None),
            Ok(RpcTarget::Id { id: "abc".into() })
        );
    }

    #[test]
    fn builds_subscription_frames() {
        let children = RpcTarget::Children { parent: "p".into() };
        assert_eq!(
            subscription_for(&children, &RpcEvents::Kinds(default_events(&children))),
            json!({"sessions":{"childrenOf":"p"},"include_workers":true,"include_context":true,"effects":true})
        );
        assert_eq!(
            subscription_for(
                &RpcTarget::Id { id: "x".into() },
                &RpcEvents::Kinds(vec!["session_status".into(), "agent_end".into()])
            ),
            json!({"sessions":{"ids":["x"]},"include_workers":true,"include_context":false,"effects":false,"mirror":["agent_end"]})
        );
        assert_eq!(
            subscription_for(&RpcTarget::All, &RpcEvents::All),
            json!({"sessions":"all","include_workers":true,"include_context":false,"effects":true,"mirror":"all"})
        );
    }

    #[test]
    fn filters_records_and_tasks() {
        assert!(!wants_record(&RpcEvents::All, &["x".into()], "x"));
        assert!(wants_record(&RpcEvents::All, &[], "x"));
        assert!(!wants_record(&RpcEvents::Kinds(vec!["a".into()]), &[], "b"));
        let task = RpcTarget::Task {
            task_id: "st_1".into(),
        };
        assert!(matches_task(&task, &json!({"context":{"task_id":"st_1"}})));
        assert!(!matches_task(&task, &json!({})));
        assert!(matches_task(&RpcTarget::All, &json!({})));
    }
}
