//! Session-held admission check (senpi `session-held.ts`): a command that would resume a session another
//! live process holds is refused with `session_held`, unless that holder's host generation has ended.
use crate::host_stalled_evidence::{HolderGeneration, holder_generation_ended};
use maho_core::{
    foreign_session_holders::foreign_session_holders,
    schedule::runner_lease::ProcessProbes,
    session_holder_records::{
        HolderGeneration as CoreHolderGeneration, SessionHolder, SessionHolderProbes,
    },
};
/// The wire code of a session-held refusal (senpi `RPC_ERROR_SESSION_HELD`).
pub const RPC_ERROR_SESSION_HELD: &str = "session_held";
/// Admission is conservative: a new command must pass the guard unless it is explicitly a read or a control needed
/// to finish or cancel an already-admitted turn (senpi `READ_ONLY_OR_CONTROL`).
pub const READ_ONLY_OR_CONTROL: [&str; 41] = [
    "get_state",
    "get_snapshot",
    "get_effect_status",
    "get_pending_inputs",
    "get_steering_messages",
    "get_follow_up_messages",
    "get_available_models",
    "get_available_thinking_levels",
    "get_model_thinking_level",
    "get_fast_mode",
    "get_session_stats",
    "resolve_path",
    "get_fork_messages",
    "get_entries",
    "get_tree",
    "get_last_assistant_text",
    "get_messages",
    "get_media",
    "get_commands",
    "get_loaded_surfaces",
    "get_auth_providers",
    "get_provider_accounts",
    "memory_report",
    "eval_cells.list",
    "eval_cells.peek",
    "eval_cells.stop",
    "export_html",
    "export_jsonl",
    "abort",
    "interrupt",
    "abort_compaction",
    "abort_branch_summary",
    "abort_retry",
    "abort_bash",
    "clear_queue",
    "check_reload_veto",
    "cleanup_bash_output",
    "set_client_info",
    "extension_ui_response",
    "extension_ui_progress",
    "host_response",
];
/// Whether a session command must pass the session-held guard before it reaches the binding (senpi
/// `dispatchSessionBinding`): everything but a read or a control of an admitted turn.
pub fn requires_session_held_check(command_type: &str) -> bool {
    !READ_ONLY_OR_CONTROL.contains(&command_type)
}
/// Whether the host generation a holder lease names has ended: positive evidence that lets an unprovable lease
/// be reclaimed instead of refusing (senpi `holderGenerationEnded`).
pub fn generation_ended(
    generation: &CoreHolderGeneration,
    env: &std::collections::HashMap<String, String>,
) -> bool {
    holder_generation_ended(
        &HolderGeneration {
            host_instance: generation.host_instance.clone(),
            host_daemon_dir: generation.host_daemon_dir.clone(),
        },
        env,
    )
}
/// Process probes plus the `holder_ended` evidence of a host generation (senpi `holderEnded: holderGenerationEnded`).
pub struct HostGenerationProbes<'a, P: ProcessProbes> {
    pub inner: &'a P,
    pub env: std::collections::HashMap<String, String>,
}
impl<'a, P: ProcessProbes> HostGenerationProbes<'a, P> {
    pub fn from_process_env(inner: &'a P) -> Self {
        Self {
            inner,
            env: std::env::vars().collect(),
        }
    }
}
#[async_trait::async_trait]
impl<P: ProcessProbes> ProcessProbes for HostGenerationProbes<'_, P> {
    fn own_identity(&self) -> maho_core::schedule::store::RunnerIdentity {
        self.inner.own_identity()
    }
    fn boot_at_ms(&self) -> u64 {
        self.inner.boot_at_ms()
    }
    fn now(&self) -> u64 {
        self.inner.now()
    }
    fn pid_alive(&self, pid: u32) -> maho_core::schedule::Result<bool> {
        self.inner.pid_alive(pid)
    }
    fn delivery_group_alive(&self, pid: u32) -> maho_core::schedule::Result<bool> {
        self.inner.delivery_group_alive(pid)
    }
    async fn process_start_ms(&self, pid: u32) -> maho_core::schedule::Result<Option<u64>> {
        self.inner.process_start_ms(pid).await
    }
}
#[async_trait::async_trait]
impl<P: ProcessProbes> SessionHolderProbes for HostGenerationProbes<'_, P> {
    async fn holder_ended(&self, generation: &CoreHolderGeneration) -> bool {
        generation_ended(generation, &self.env)
    }
}
/// Holders of `session_file` outside this host's own process family, or `Ok(())` when none: the foreign-holder
/// read passes `holder_ended` so an unprovable lease whose host generation ended is reclaimed rather than reported.
pub async fn session_held_check<P: ProcessProbes>(
    session_file: Option<&str>,
    session_id: Option<&str>,
    probes: &P,
    daemon_pids: &[u32],
) -> Result<(), Vec<SessionHolder>> {
    let probes = HostGenerationProbes::from_process_env(probes);
    let holders = foreign_session_holders(session_file, session_id, &probes)
        .await
        .unwrap_or_default();
    let foreign: Vec<SessionHolder> = holders
        .into_iter()
        .filter(|holder| !daemon_pids.contains(&holder.pid))
        .collect();
    if foreign.is_empty() {
        Ok(())
    } else {
        Err(foreign)
    }
}
/// The `session_held` error detail: each holder's pid plus the optional cwd, host socket and host instance.
pub fn holders_detail(holders: &[SessionHolder]) -> serde_json::Value {
    let holders: Vec<serde_json::Value> = holders
        .iter()
        .map(|holder| {
            let mut item = serde_json::json!({"pid": holder.pid});
            if let Some(cwd) = &holder.cwd {
                item["cwd"] = cwd.clone().into();
            }
            if let Some(socket) = &holder.host_socket {
                item["host_socket"] = socket.clone().into();
            }
            if let Some(instance) = &holder.host_instance {
                item["host_instance"] = instance.clone().into();
            }
            item
        })
        .collect();
    serde_json::json!({"holders": holders})
}
#[cfg(test)]
mod tests {
    use super::*;
    /// get-snapshot.test.ts / senpi#4266: `get_snapshot` is a read, so it neither passes the held guard nor
    /// refreshes the session's activity; a prompt does both.
    #[test]
    fn get_snapshot_is_a_read_for_admission_and_activity() {
        assert!(!requires_session_held_check("get_snapshot"));
        assert!(!requires_session_held_check("abort"));
        assert!(requires_session_held_check("prompt"));
        assert!(requires_session_held_check("set_model"));
        assert!(!crate::session_command_activity::refreshes_session_activity("get_snapshot"));
        assert!(crate::session_command_activity::refreshes_session_activity(
            "prompt"
        ));
    }
    #[test]
    fn holder_detail_omits_absent_fields() {
        let detail = holders_detail(&[SessionHolder {
            pid: 7,
            cwd: None,
            host_socket: None,
            host_instance: Some("i".into()),
        }]);
        assert_eq!(
            detail,
            serde_json::json!({"holders":[{"pid":7,"host_instance":"i"}]})
        );
    }
    #[test]
    fn an_unnamed_generation_never_ends() {
        let generation = CoreHolderGeneration {
            host_instance: None,
            host_daemon_dir: Some("/d".into()),
        };
        assert!(!generation_ended(&generation, &Default::default()));
    }
    #[test]
    fn this_hosts_own_generation_never_ends() {
        let env = std::collections::HashMap::from([(
            crate::protocol_identity::HOST_INSTANCE_ID_ENV.to_owned(),
            "own".to_owned(),
        )]);
        let generation = CoreHolderGeneration {
            host_instance: Some("own".into()),
            host_daemon_dir: Some("/d".into()),
        };
        assert!(!generation_ended(&generation, &env));
    }
}
