//! The per-generation barrier the tool-child client owns; port of `client-barrier.ts`.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::channel::ToolChildChannel;
use crate::descendants::{DescendantsLedger, LaunchLog, generation_ledger_path};
use crate::group_ownership::{
    LedgeredGroup, SweepOptions, SweepOutcome, default_group_probes, sweep_groups,
};
use crate::launch_enforcer::{BarrierViolation, LaunchEnforcer, LaunchEnforcerOptions};
use crate::protocol::{ChildToServer, ServerToChild, ToolChildMessage};
use crate::test_controls::{
    PrefixWatch, TEST_CONTROL_DIR_ENV, TestSwitch, process_env, read_test_controls,
};

const SLOT_QUERY_PREFIX: &str = "query-slot-";

pub struct GenerationBarrier {
    pub ledger: Arc<DescendantsLedger>,
    pub log: Arc<LaunchLog>,
    generation: u64,
    channel: Arc<ToolChildChannel>,
    enforcer: LaunchEnforcer,
    drop_control_before_ack: bool,
    slot_queries: Mutex<Option<PrefixWatch>>,
}

pub struct GenerationBarrierOptions {
    pub dir: PathBuf,
    pub generation: u64,
    pub channel: Arc<ToolChildChannel>,
    pub on_violation: Arc<dyn Fn(BarrierViolation) + Send + Sync>,
    pub hold_ack: Option<Arc<crate::launch_enforcer::HoldAckFn>>,
    pub confirm_member: Option<Arc<dyn Fn(i64) -> bool + Send + Sync>>,
}

/// Must be called inside a tokio runtime (the launch log runs a writer task).
pub fn create_generation_barrier(options: GenerationBarrierOptions) -> GenerationBarrier {
    let env = process_env();
    let controls = read_test_controls(&env);
    let hold_ack = options.hold_ack.or_else(|| {
        if controls.enabled(TestSwitch::HoldLaunchAck) {
            let controls = controls.clone();
            let hold: Arc<
                dyn Fn(String) -> futures::future::BoxFuture<'static, io::Result<()>> + Send + Sync,
            > = Arc::new(move |launch_id: String| {
                let controls = controls.clone();
                Box::pin(async move { controls.released(&format!("release-{launch_id}")).await })
            });
            Some(hold)
        } else {
            None
        }
    });
    let ledger = Arc::new(DescendantsLedger::new(generation_ledger_path(
        &options.dir,
        options.generation,
    )));
    let log = Arc::new(LaunchLog::new(options.dir.join("launches.jsonl")));
    let ack_channel = Arc::clone(&options.channel);
    let probes = default_group_probes();
    let enforcer = LaunchEnforcer::new(LaunchEnforcerOptions {
        generation: options.generation,
        ledger: Arc::clone(&ledger),
        log: Arc::clone(&log),
        send_ack: Arc::new(move |launch_id| {
            let _ = ack_channel.send(&ToolChildMessage::Server(ServerToChild::LaunchAck {
                launch_id: launch_id.to_owned(),
            }));
        }),
        on_violation: options.on_violation,
        leader_start: Arc::clone(&probes.start_of),
        hold_ack,
        set_timer: None,
        proof_ms: None,
        confirm_member: options.confirm_member,
    });
    // Test-only: with SENPI_TOOL_CHILD_TRACE=1 a control file `query-slot-<launchId>.<n>` asks the child for that launch's
    // slot counts; the reply (`slot_state`) lands in launches.jsonl beside the launch it describes.
    let slot_queries = if env.get("SENPI_TOOL_CHILD_TRACE").map(String::as_str) == Some("1")
        && env.contains_key(TEST_CONTROL_DIR_ENV)
    {
        let channel = Arc::clone(&options.channel);
        Some(controls.watch_prefix(SLOT_QUERY_PREFIX, move |name| {
            let rest = &name[SLOT_QUERY_PREFIX.len()..];
            // `.replace(/\.[^.]*$/, "")`: drop the last `.<suffix>`.
            let launch_id = match rest.rfind('.') {
                Some(at) => &rest[..at],
                None => rest,
            };
            let _ = channel.send(&ToolChildMessage::Server(ServerToChild::SlotQuery {
                launch_id: launch_id.to_owned(),
            }));
        }))
    } else {
        None
    };
    GenerationBarrier {
        ledger,
        log,
        generation: options.generation,
        channel: options.channel,
        enforcer,
        drop_control_before_ack: controls.enabled(TestSwitch::DropControlBeforeAck),
        slot_queries: Mutex::new(slot_queries),
    }
}

impl GenerationBarrier {
    /// True when the barrier consumed the frame. `launch` is handled on a spawned task (`void enforcer.onLaunch`).
    pub fn handle(&self, frame: &ToolChildMessage) -> bool {
        let ToolChildMessage::Child(child) = frame else {
            return false;
        };
        match child {
            ChildToServer::Launch { .. } => {
                if self.drop_control_before_ack {
                    self.channel.close();
                    return true;
                }
                let enforcer = self.enforcer.clone();
                let frame = child.clone();
                tokio::spawn(async move {
                    let _ = enforcer.on_launch(&frame).await;
                });
                true
            }
            ChildToServer::LaunchProof { .. } => {
                self.enforcer.on_proof(child);
                true
            }
            ChildToServer::LaunchDone { .. } => {
                self.enforcer.on_done(child);
                true
            }
            ChildToServer::CommandStarted { .. } => {
                self.enforcer.on_command_started(child);
                true
            }
            ChildToServer::SlotState { .. } => {
                if let Ok(value) = serde_json::to_value(child) {
                    drop(self.log.record(value));
                }
                true
            }
            _ => false,
        }
    }

    pub async fn teardown(&self, child: Option<LedgeredGroup>) -> Vec<SweepOutcome> {
        let watch = self
            .slot_queries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(watch) = watch {
            watch.close();
        }
        self.enforcer.recorded().await;
        self.enforcer.dispose();
        let mut owned: Vec<LedgeredGroup> = self
            .ledger
            .entries()
            .into_iter()
            .filter_map(|entry| {
                entry.pgid.map(|pgid| LedgeredGroup {
                    launch_id: entry.launch_id,
                    pgid,
                    leader_start: entry.leader_start,
                    lifetime: entry.lifetime.into(),
                })
            })
            .collect();
        let outcomes = if cfg!(windows) {
            Vec::new()
        } else {
            owned.extend(child);
            sweep_groups(
                &owned,
                SweepOptions {
                    grace_ms: 2_000,
                    kill_wait_ms: 2_000,
                    poll_ms: None,
                },
                &default_group_probes(),
            )
            .await
        };
        drop(self.log.record(json!({
            "type": "teardown",
            "generation": self.generation,
            "outcomes": outcomes
                .iter()
                .map(|o| format!("{}:{}", o.launch_id, o.verdict.as_str()))
                .collect::<Vec<_>>()
                .join(","),
        })));
        self.log.settled().await;
        let _ = self.ledger.remove().await;
        outcomes
    }
}
