//! The `checkpointed-rpc` durability class (senpi `durable-rpc.ts`): a persistent rpc watch comes
//! back under its original `mon_` id, re-resolves its target, re-subscribes, and reports once,
//! through the same sink, what changed while nothing watched: a session whose status moved, one
//! that is gone, a host whose generation changed. `seq` is never compared across hosts or
//! generations; only statuses and generations are.
//!
//! Native shape: the restore dispatch runs synchronously under the manager/registry locks, so the
//! handler arms the record at once (`RpcMonitorSource::arm`) and spawns the endpoint join; the
//! detached-change lines arrive once that join settles, as upstream's awaited register would.
use crate::restore::{RestoreOutcome, reapply_persisted_mute};
use crate::rpc_monitor_source::{RegisterRpcMonitorOptions, RpcCheckpointSink};
use crate::rpc_monitor_targets::{default_events, parse_rpc_target};
use crate::terminal_manifest_model::{ManifestMonitor, MonitorRuntimeKind};

/// Upstream `RestoreHandlerResult` for this handler: the outcome and why a watch was lost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcRestoreResult {
    pub outcome: RestoreOutcome,
    pub reason: Option<String>,
}

fn lost(reason: String) -> RpcRestoreResult {
    RpcRestoreResult {
        outcome: RestoreOutcome::Lost,
        reason: Some(reason),
    }
}

/// Upstream `CheckpointedRpcRestoreDeps`.
pub struct CheckpointedRpcRestoreDeps<'a> {
    /// Arms the watch: the registry's rpc source in production; returns the fresh runtime id.
    pub arm: &'a mut dyn FnMut(RegisterRpcMonitorOptions) -> std::io::Result<String>,
    /// Pauses runtime ids (a persisted mute).
    pub pause: &'a mut dyn FnMut(&[String]),
    /// The writer's `scheduleRpcCheckpoint`.
    pub writer: Option<RpcCheckpointSink>,
    pub bind_monitor_id: Option<&'a mut dyn FnMut(&str, &str)>,
    /// This session's durable id, which a `children` target names.
    pub session_id: Option<String>,
    pub agent_dir: Option<std::path::PathBuf>,
}

/// Upstream `createCheckpointedRpcRestoreHandler(deps)(monitor, context)`; `remaining_ms` is
/// `RestoreContext.remainingMs`.
pub fn restore_checkpointed_rpc(
    monitor: &ManifestMonitor,
    remaining_ms: Option<f64>,
    deps: CheckpointedRpcRestoreDeps<'_>,
) -> RpcRestoreResult {
    let (MonitorRuntimeKind::Rpc, Some(target_text)) = (monitor.runtime_kind, &monitor.target)
    else {
        return lost(format!(
            "monitor {} is not a restorable rpc watch",
            monitor.monitor_id
        ));
    };
    let target = match parse_rpc_target(target_text, deps.session_id.as_deref()) {
        Ok(target) => target,
        Err(error) => return lost(error),
    };
    let filter = match &monitor.filter {
        None => None,
        Some(text) => match fancy_regex::Regex::new(text) {
            Ok(regex) => Some((text.clone(), regex)),
            Err(_) => {
                return lost(format!(
                    "monitor {} has an invalid filter",
                    monitor.monitor_id
                ));
            }
        },
    };
    let events = monitor
        .events
        .clone()
        .unwrap_or_else(|| crate::rpc_monitor_targets::RpcEvents::Kinds(default_events(&target)));
    let options = RegisterRpcMonitorOptions {
        description: monitor.description.clone(),
        target,
        target_text: target_text.clone(),
        events,
        exclude: monitor.exclude.clone().unwrap_or_default(),
        filter,
        persistent: monitor.persistent,
        timeout_ms: remaining_ms.map(|ms| ms.max(1.0) as u64),
        expires_at: monitor.expires_at,
        monitor_id: Some(monitor.monitor_id.clone()),
        agent_dir: deps
            .agent_dir
            .unwrap_or_else(|| maho_core::config::get_agent_dir().into()),
        endpoints: None,
        opener: None,
        restored_from: monitor.rpc_checkpoint.clone(),
        on_checkpoint: deps.writer,
    };
    match (deps.arm)(options) {
        Ok(id) => {
            if let Some(bind) = deps.bind_monitor_id {
                bind(&monitor.monitor_id, &id);
            }
            // A watch muted before the restart stays muted, as a file watch does.
            RpcRestoreResult {
                outcome: reapply_persisted_mute(monitor, &id, deps.pause),
                reason: None,
            }
        }
        Err(error) => lost(format!("cannot re-watch {target_text}: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc_monitor_targets::{RpcEvents, RpcTarget};
    use serde_json::json;

    fn checkpoint() -> serde_json::Value {
        json!({"hosts":{"/tmp/h.sock":{"instanceId":"i-1","generation":"7"}},"statuses":{"/tmp/h.sock#s1":"running"}})
    }
    fn parsed_entry() -> ManifestMonitor {
        let entry = json!({"monitorId":"mon_RPC","sessionId":"owner","description":"children status",
            "runtimeKind":"rpc","durabilityClass":"checkpointed-rpc","target":"children",
            "events":["session_status","effect_status"],"exclude":[],"createdAt":1,
            "expiresAt":10_000_000_000_000_u64,"persistent":true,"suspended":false,"lastCheckpoint":null,
            "deliveryPaused":false,"fireWindow":{"startMs":1,"count":0},"rpcCheckpoint":checkpoint()});
        crate::restore::parse_terminal_manifest(
            &json!({"monitors":[entry],"backgroundSessions":[],"updatedAt":2}),
            "owner",
        )
        .unwrap()
        .monitors
        .remove(0)
    }

    #[test]
    fn keeps_the_rpc_spec_and_checkpoint_through_the_strict_manifest_parse() {
        let entry = parsed_entry();
        assert_eq!(entry.runtime_kind, MonitorRuntimeKind::Rpc);
        assert_eq!(
            entry.durability_class,
            crate::terminal_manifest_model::MonitorDurabilityClass::CheckpointedRpc
        );
        assert_eq!(entry.target.as_deref(), Some("children"));
        assert_eq!(
            entry.events,
            Some(RpcEvents::Kinds(vec![
                "session_status".into(),
                "effect_status".into()
            ]))
        );
        assert_eq!(
            serde_json::to_value(&entry.rpc_checkpoint).unwrap(),
            checkpoint()
        );
        // The entry writes back the same rpc fields it was read from.
        let written = serde_json::to_value(&entry).unwrap();
        assert_eq!(
            written["events"],
            json!(["session_status", "effect_status"])
        );
        assert_eq!(written["rpcCheckpoint"], checkpoint());
    }

    #[test]
    fn re_arms_the_watch_under_its_mon_id_with_the_saved_checkpoint_and_rebinds_the_runtime_id() {
        let mut armed = Vec::new();
        let mut bound = Vec::new();
        let mut arm = |options: RegisterRpcMonitorOptions| {
            armed.push(options);
            Ok("rpc_9".to_owned())
        };
        let mut bind = |monitor_id: &str, runtime_id: &str| {
            bound.push((monitor_id.to_owned(), runtime_id.to_owned()))
        };
        let result = restore_checkpointed_rpc(
            &parsed_entry(),
            None,
            CheckpointedRpcRestoreDeps {
                arm: &mut arm,
                pause: &mut |_| {},
                writer: None,
                bind_monitor_id: Some(&mut bind),
                session_id: Some("parent-durable".into()),
                agent_dir: Some("/tmp/agent".into()),
            },
        );
        assert_eq!(
            result,
            RpcRestoreResult {
                outcome: RestoreOutcome::Restored,
                reason: None
            }
        );
        assert_eq!(armed[0].monitor_id.as_deref(), Some("mon_RPC"));
        assert_eq!(
            armed[0].target,
            RpcTarget::Children {
                parent: "parent-durable".into()
            }
        );
        assert!(armed[0].persistent);
        assert_eq!(
            serde_json::to_value(&armed[0].restored_from).unwrap(),
            checkpoint()
        );
        assert_eq!(bound, vec![("mon_RPC".to_owned(), "rpc_9".to_owned())]);
    }

    #[test]
    fn loses_an_entry_that_is_not_an_rpc_watch_instead_of_arming_it() {
        let mut entry = parsed_entry();
        entry.runtime_kind = MonitorRuntimeKind::File;
        let mut arm = |_: RegisterRpcMonitorOptions| -> std::io::Result<String> {
            panic!("an rpc arm for a file entry")
        };
        let result = restore_checkpointed_rpc(
            &entry,
            None,
            CheckpointedRpcRestoreDeps {
                arm: &mut arm,
                pause: &mut |_| {},
                writer: None,
                bind_monitor_id: None,
                session_id: None,
                agent_dir: Some("/tmp/agent".into()),
            },
        );
        assert_eq!(result.outcome, RestoreOutcome::Lost);
    }

    #[test]
    fn a_watch_muted_before_the_restart_comes_back_muted() {
        let mut entry = parsed_entry();
        entry.delivery_paused = true;
        let mut paused = Vec::new();
        let mut arm = |_: RegisterRpcMonitorOptions| Ok("rpc_9".to_owned());
        let mut pause = |ids: &[String]| paused.push(ids.to_vec());
        let result = restore_checkpointed_rpc(
            &entry,
            None,
            CheckpointedRpcRestoreDeps {
                arm: &mut arm,
                pause: &mut pause,
                writer: None,
                bind_monitor_id: None,
                session_id: Some("owner".into()),
                agent_dir: Some("/tmp/agent".into()),
            },
        );
        assert_eq!(result.outcome, RestoreOutcome::Muted);
        assert_eq!(paused, vec![vec!["rpc_9".to_owned()]]);
    }
}
