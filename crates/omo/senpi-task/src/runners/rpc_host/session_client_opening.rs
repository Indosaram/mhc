//! `rpc-host/session-client-opening.ts` + the `spawnedBy` half of `session-transport.ts`
//! (`HostSessionOpenInput.spawnedBy` / `toWireOpen`) and `open-session.ts` (`rpcChildAncestry`).
//!
//! The mhc assembly builds the `open_session` JSON itself (maho-cli `task_host.rs`); it calls
//! [`with_spawned_by`] on that request with the host's `get_protocol_info` engine version.

use serde_json::Value;

use crate::runners::child_ancestry::rpc_child_ancestry;
use crate::runners::engine_spawned_by::engine_accepts_spawned_by;
use crate::runners::types::RpcRunnerSpec;

/// Adds the child's ancestry to an `open_session` request. No capability names `spawnedBy`, so the
/// host's engine version decides; an unproven host opens the child without ancestry (logged).
/// Returns whether the field was sent.
pub fn with_spawned_by(
    request: &mut Value,
    spec: &RpcRunnerSpec,
    host_engine_version: Option<&str>,
    socket: &str,
) -> bool {
    let Some(spawned_by) = rpc_child_ancestry(spec) else {
        return false;
    };
    if !engine_accepts_spawned_by(host_engine_version) {
        utils::logger::log(
            "senpi-task host engine does not accept spawnedBy; opening the child without ancestry",
            Some(&serde_json::json!({"socket": socket, "engineVersion": host_engine_version})),
        );
        return false;
    }
    request["spawnedBy"] = serde_json::json!(spawned_by);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // rpc-host-spawned-by.test.ts
    #[test]
    fn open_carries_spawned_by_only_for_an_engine_that_accepts_it() {
        let spec = RpcRunnerSpec {
            task_id: "worker-task".into(),
            parent_session_id: Some("nested-parent".into()),
            root_session_id: Some("tree-root".into()),
            ..RpcRunnerSpec::default()
        };
        let mut accepted = json!({"sessionPath": "p", "cwd": "/w"});
        assert!(with_spawned_by(
            &mut accepted,
            &spec,
            Some("2026.10.25-2"),
            "sock"
        ));
        assert_eq!(
            accepted["spawnedBy"],
            json!({"parentSessionId": "nested-parent", "rootSessionId": "tree-root", "taskId": "worker-task"})
        );
        for version in [Some("2026.10.24-3"), Some("dev-build"), None] {
            let mut dropped = json!({"sessionPath": "p", "cwd": "/w"});
            assert!(!with_spawned_by(&mut dropped, &spec, version, "sock"));
            assert!(dropped.get("spawnedBy").is_none());
        }
        let orphan = RpcRunnerSpec::default();
        let mut none = json!({});
        assert!(!with_spawned_by(
            &mut none,
            &orphan,
            Some("2026.10.25-2"),
            "sock"
        ));
    }
}
