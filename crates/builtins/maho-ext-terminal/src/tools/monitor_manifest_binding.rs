//! Port of senpi `terminal/tools/monitor-manifest-binding.ts`: the per-session manifest-writer
//! binding the monitor tool hands captured specs to, the bounded queue of specs registered before a
//! writer binds, and durable admission control.
//!
//! Native shape: upstream keeps a process-wide `Map<sessionId, TerminalManifestWriter>`. Here the
//! writer is owned by the session's `TerminalStateLifecycle` behind an async mutex, so the binding
//! map only tracks which session keys have a bound writer (`bindTerminalManifestWriter` /
//! `unbindTerminalManifestWriter`) plus the pending specs; draining hands the queued specs back to
//! the caller that holds the writer, which records them in order.
use super::context::{TerminalToolResult, error_result};
use crate::terminal_manifest::TerminalManifestWriter;
use crate::terminal_manifest_model::MonitorRegistration;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Mutex, OnceLock};

/// Specs captured before a writer is bound are queued per session; past the cap the oldest is dropped.
pub const MAX_PENDING_MONITOR_SPECS: usize = 32;

#[derive(Default)]
struct Bindings {
    bound: BTreeSet<String>,
    pending: BTreeMap<String, VecDeque<MonitorRegistration>>,
    /// The bound writer's `scheduleRpcCheckpoint`, looked up at call time like upstream's map.
    rpc_sinks: BTreeMap<String, crate::rpc_monitor_source::RpcCheckpointSink>,
}

fn bindings() -> std::sync::MutexGuard<'static, Bindings> {
    static BINDINGS: OnceLock<Mutex<Bindings>> = OnceLock::new();
    BINDINGS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `bindTerminalManifestWriter`: mark the session's writer bound and drain the specs queued before
/// it, in registration order. The caller records each through the writer it holds.
pub fn bind_terminal_manifest_writer(session_id: &str) -> Vec<MonitorRegistration> {
    let mut bindings = bindings();
    bindings.bound.insert(session_id.to_owned());
    bindings
        .pending
        .remove(session_id)
        .map(Vec::from)
        .unwrap_or_default()
}

/// `bindTerminalManifestWriter` followed by the drain: every queued spec is recorded in order.
pub async fn bind_and_drain(session_id: &str, writer: &mut TerminalManifestWriter, now: f64) {
    let drained = bind_terminal_manifest_writer(session_id);
    bindings()
        .rpc_sinks
        .insert(session_id.to_owned(), writer.rpc_checkpoint_sink());
    #[cfg(test)]
    {
        let hook = after_bind_hooks().remove(session_id);
        if let Some(hook) = hook {
            hook();
        }
    }
    for registration in drained {
        writer.record_register(registration, now).await;
    }
}

/// Test seam: a one-shot callback run right after a session's writer is bound, before the caller
/// installs the lifecycle that holds it, so a test can interleave a monitor call at that instant.
#[cfg(test)]
type AfterBindHook = Box<dyn FnOnce() + Send>;
#[cfg(test)]
fn after_bind_hooks() -> std::sync::MutexGuard<'static, BTreeMap<String, AfterBindHook>> {
    static HOOKS: OnceLock<Mutex<BTreeMap<String, AfterBindHook>>> = OnceLock::new();
    HOOKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
#[cfg(test)]
pub(crate) fn set_after_bind_hook(session_id: &str, hook: AfterBindHook) {
    after_bind_hooks().insert(session_id.to_owned(), hook);
}

pub fn unbind_terminal_manifest_writer(session_id: &str) {
    let mut bindings = bindings();
    bindings.bound.remove(session_id);
    bindings.rpc_sinks.remove(session_id);
}

pub fn is_terminal_manifest_writer_bound(session_id: &str) -> bool {
    bindings().bound.contains(session_id)
}

pub fn pending_durable_spec_count(session_id: &str) -> usize {
    bindings().pending.get(session_id).map_or(0, |queue| {
        queue
            .iter()
            .filter(|registration| registration.spec.persistent())
            .count()
    })
}

/// `manifestSessionKey`: the durability session key for a tool context, when it carries one.
pub fn manifest_session_key(
    ctx: Option<&dyn maho_tools::definition::ToolContext>,
) -> Option<String> {
    let ctx = ctx?;
    let id = ctx.session_manager().session_id();
    (!id.is_empty()).then(|| id.to_owned())
}

/// `handMonitorSpec`: hand a spec captured at the monitor call site to the session's bound writer,
/// or queue it until one binds. Returns the spec back when a writer is bound (the caller records
/// it, awaited, so the tool result never claims a watch before its manifest entry exists).
pub fn hand_monitor_spec(
    session_key: Option<&str>,
    registration: MonitorRegistration,
) -> Option<MonitorRegistration> {
    let session_key = session_key?;
    let mut bindings = bindings();
    if bindings.bound.contains(session_key) {
        return Some(registration);
    }
    let queue = bindings.pending.entry(session_key.to_owned()).or_default();
    queue.push_back(registration);
    if queue.len() > MAX_PENDING_MONITOR_SPECS {
        queue.pop_front();
    }
    None
}

/// `handFileCheckpoint`: persist a durable file watch's baseline through the writer's debounced path.
pub fn hand_file_checkpoint(
    writer: Option<&mut TerminalManifestWriter>,
    monitor_id: &str,
    registry: &crate::monitor_registry::MonitorRegistry,
    runtime_id: &str,
) {
    if let Some(writer) = writer
        && let Some(checkpoint) = registry.file_checkpoint(runtime_id)
    {
        writer.schedule_checkpoint(monitor_id, checkpoint);
    }
}

/// `handRpcCheckpoint`: persist an rpc watch's view (host generations, session statuses) through
/// the writer bound for the session at call time (so a reload's rebind is followed). Without a
/// bound writer the checkpoint is dropped, as upstream.
pub fn hand_rpc_checkpoint(
    session_key: Option<&str>,
    monitor_id: &str,
    checkpoint: crate::terminal_manifest_model::TerminalManifestRpcCheckpoint,
) {
    let Some(session_key) = session_key else {
        return;
    };
    let sink = bindings().rpc_sinks.get(session_key).cloned();
    if let Some(sink) = sink {
        sink(monitor_id, checkpoint);
    }
}

/// `durableAdmissionError`: no cap unless `terminal.maxDurableMonitors` sets one; then refuse once
/// the session already holds that many restart-surviving monitors, counting the bound writer's
/// entries and the specs still queued for a writer. Checked before any spawn or registration. A
/// context with no session key persists nothing, so it has no durable population to cap.
pub fn durable_admission_error(
    max_durable_monitors: Option<usize>,
    session_key: Option<&str>,
    writer: Option<&TerminalManifestWriter>,
) -> Option<TerminalToolResult> {
    let limit = max_durable_monitors?;
    let session_key = session_key?;
    let held = writer.map_or(0, TerminalManifestWriter::durable_count)
        + pending_durable_spec_count(session_key);
    if held < limit {
        return None;
    }
    Some(error_result(format!(
        "Cannot start another persistent monitor: this session already holds {limit} durable monitors, the limit set by terminal.maxDurableMonitors. Stop one with kill_bash first, or raise the setting."
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_manifest_model::{MonitorRegistration, MonitorSpec};

    fn spec(monitor_id: &str, persistent: bool) -> MonitorRegistration {
        MonitorRegistration {
            monitor_id: monitor_id.to_owned(),
            spec: MonitorSpec::Command {
                description: "watch".to_owned(),
                command: "true".to_owned(),
                filter: None,
                human_input: None,
                wakes_per_hour: None,
                cwd: None,
                persistent,
            },
        }
    }

    #[test]
    fn specs_queue_until_a_writer_binds_and_drain_in_order() {
        let key = "binding-queue";
        assert!(hand_monitor_spec(Some(key), spec("mon_1", true)).is_none());
        assert!(hand_monitor_spec(Some(key), spec("mon_2", false)).is_none());
        assert_eq!(pending_durable_spec_count(key), 1);
        let drained = bind_terminal_manifest_writer(key);
        assert_eq!(
            drained
                .iter()
                .map(|registration| registration.monitor_id.as_str())
                .collect::<Vec<_>>(),
            ["mon_1", "mon_2"]
        );
        assert_eq!(pending_durable_spec_count(key), 0);
        // Bound: the spec comes straight back to the caller that holds the writer.
        assert!(hand_monitor_spec(Some(key), spec("mon_3", true)).is_some());
        unbind_terminal_manifest_writer(key);
        assert!(!is_terminal_manifest_writer_bound(key));
        assert!(hand_monitor_spec(Some(key), spec("mon_4", true)).is_none());
        assert_eq!(bind_terminal_manifest_writer(key).len(), 1);
        unbind_terminal_manifest_writer(key);
        assert!(hand_monitor_spec(None, spec("mon_5", true)).is_none());
    }

    /// `handRpcCheckpoint`: only the writer bound at call time receives the checkpoint; it is
    /// absorbed into that writer's entry on the next flush.
    #[tokio::test]
    async fn an_rpc_checkpoint_reaches_only_the_bound_writer() {
        use crate::terminal_manifest_model::MonitorSpec;
        let dir = tempfile::tempdir().unwrap();
        let key = "binding-rpc-checkpoint";
        let mut writer = TerminalManifestWriter::new(dir.path(), key);
        let registration = MonitorRegistration {
            monitor_id: "mon_RPC".to_owned(),
            spec: MonitorSpec::Rpc {
                description: "kids".into(),
                target: "all".into(),
                events: None,
                exclude: None,
                filter: None,
                persistent: true,
            },
        };
        writer.record_register(registration, 1.0).await;
        let checkpoint = |status: &str| {
            serde_json::from_value::<crate::terminal_manifest_model::TerminalManifestRpcCheckpoint>(
                serde_json::json!({"hosts":{},"statuses":{"/h#s1":status}}),
            )
            .unwrap()
        };
        hand_rpc_checkpoint(Some(key), "mon_RPC", checkpoint("unbound"));
        bind_and_drain(key, &mut writer, 2.0).await;
        hand_rpc_checkpoint(Some(key), "mon_RPC", checkpoint("busy"));
        hand_rpc_checkpoint(None, "mon_RPC", checkpoint("nokey"));
        writer.flush(3.0).await;
        unbind_terminal_manifest_writer(key);
        hand_rpc_checkpoint(Some(key), "mon_RPC", checkpoint("after-unbind"));
        writer.flush(4.0).await;
        let saved = writer.store.read().await.unwrap().unwrap();
        assert_eq!(
            saved["monitors"][0]["rpcCheckpoint"]["statuses"]["/h#s1"],
            "busy"
        );
    }

    #[test]
    fn the_pending_queue_is_bounded_and_drops_the_oldest() {
        let key = "binding-bounded";
        for index in 0..MAX_PENDING_MONITOR_SPECS + 3 {
            assert!(hand_monitor_spec(Some(key), spec(&format!("mon_{index}"), true)).is_none());
        }
        let drained = bind_terminal_manifest_writer(key);
        assert_eq!(drained.len(), MAX_PENDING_MONITOR_SPECS);
        assert_eq!(drained[0].monitor_id, "mon_3");
        unbind_terminal_manifest_writer(key);
    }

    /// terminal-durable-admission.test.ts: no cap without the setting; with a cap the queued and
    /// written durable monitors both count, ephemeral ones never do.
    #[tokio::test]
    async fn admission_counts_written_and_queued_durable_specs_only_under_a_cap() {
        let dir = tempfile::tempdir().unwrap();
        let key = "binding-admission";
        let mut writer = TerminalManifestWriter::new(dir.path(), key);
        for index in 0..12 {
            writer
                .record_register(spec(&format!("mon_w{index}"), true), 1.0)
                .await;
        }
        assert!(durable_admission_error(None, Some(key), Some(&writer)).is_none());
        let mut capped = TerminalManifestWriter::new(dir.path(), "binding-admission-capped");
        capped.record_register(spec("mon_a", true), 1.0).await;
        capped.record_register(spec("mon_e", false), 1.0).await;
        let capped_key = "binding-admission-capped";
        assert!(durable_admission_error(Some(3), Some(capped_key), Some(&capped)).is_none());
        assert!(hand_monitor_spec(Some(capped_key), spec("mon_q1", true)).is_none());
        assert!(hand_monitor_spec(Some(capped_key), spec("mon_q2", false)).is_none());
        assert!(durable_admission_error(Some(3), Some(capped_key), Some(&capped)).is_none());
        assert!(hand_monitor_spec(Some(capped_key), spec("mon_q3", true)).is_none());
        let refused = durable_admission_error(Some(3), Some(capped_key), Some(&capped)).unwrap();
        assert_eq!(refused.is_error, Some(true));
        assert!(
            refused.content[0]
                .text
                .contains("terminal.maxDurableMonitors")
        );
        // No session key: nothing is persisted, so nothing is capped.
        assert!(durable_admission_error(Some(1), None, Some(&capped)).is_none());
        bind_terminal_manifest_writer(capped_key);
        unbind_terminal_manifest_writer(capped_key);
    }
}
