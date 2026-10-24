//! The child half of the execution barrier (senpi#3566, plan todo 4). Every launch path asks the gate for a slot, which
//! sends `launch` with the containment handle; the server records it and acknowledges. The launch path then stores its
//! invoke tick immediately before the real call and calls `invoking`, and the gate publishes `launch_proof` from the
//! shared clock without waiting for the call to return. `done` reports completion separately, with no deadline.
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::oneshot;

use crate::barrier_clock::{BarrierClock, BarrierClockError, DEFAULT_LAUNCH_SLOTS, SlotState};

const FINISHED_SLOT_STATES: usize = 256;

use crate::channel::ChannelSend;
use crate::launch_env_names::ProcessEnv;
use crate::protocol::ChildToServer;
use crate::protocol::{
    LaunchApi, LaunchContainment, LaunchLifetime, LaunchOrigin, ServerToChild, ToolChildMessage,
};
use crate::test_controls::{TestControls, TestSwitch, read_test_controls};
use crate::windows_job::current_process_in_job;
use crate::worker_launch::WorkerLaunchMessage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRequest {
    pub job_id: String,
    pub origin: LaunchOrigin,
    pub api: LaunchApi,
    pub containment: LaunchContainment,
    pub lifetime: LaunchLifetime,
    pub pid: Option<u32>,
    pub pgid: Option<u32>,
}

pub struct GatedLaunch {
    pub launch_id: String,
    pub slot: usize,
    /// Resolves when the server's `launch_ack` has been recorded on the shared clock.
    pub acked: oneshot::Receiver<()>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct LaunchGateError {
    pub code: String,
    pub message: String,
}

impl LaunchGateError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

struct SlotEntry {
    slot: usize,
    ack: Option<oneshot::Sender<()>>,
    lifetime: LaunchLifetime,
    request: LaunchRequest,
}

#[derive(Default)]
struct GateState {
    slots: HashMap<String, SlotEntry>,
    /// Final slot state of recent finished launches, so a test query after `launch_done` still reads the counts.
    finished: HashMap<String, SlotState>,
    finished_order: VecDeque<String>,
    next: u64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn launch_message(launch_id: &str, request: &LaunchRequest) -> ToolChildMessage {
    ToolChildMessage::Child(ChildToServer::Launch {
        launch_id: launch_id.to_string(),
        job_id: request.job_id.clone(),
        origin: request.origin,
        api: request.api,
        containment: request.containment,
        lifetime: request.lifetime,
        pid: request.pid.map(i64::from),
        pgid: request.pgid.map(i64::from),
    })
}

pub struct LaunchGate {
    pub clock: BarrierClock,
    channel: Arc<dyn ChannelSend>,
    state: Mutex<GateState>,
    controls: Arc<TestControls>,
    creator_in_job: Box<dyn Fn() -> bool + Send + Sync>,
}

impl LaunchGate {
    /// `new LaunchGate(channel, clock = BarrierClock.create(), env = process.env, creatorInJob = currentProcessInJob)`:
    /// `None` selects the upstream default for `clock` and `creator_in_job`.
    pub fn new(
        channel: Arc<dyn ChannelSend>,
        clock: Option<BarrierClock>,
        env: &ProcessEnv,
        creator_in_job: Option<Box<dyn Fn() -> bool + Send + Sync>>,
    ) -> Result<Self, BarrierClockError> {
        let clock = match clock {
            Some(clock) => clock,
            None => BarrierClock::create(DEFAULT_LAUNCH_SLOTS)?,
        };
        Ok(Self {
            clock,
            channel,
            state: Mutex::new(GateState::default()),
            controls: Arc::new(read_test_controls(env)),
            creator_in_job: creator_in_job
                .unwrap_or_else(|| Box::new(|| current_process_in_job(None))),
        })
    }

    /// Windows: the command a launch started, with whether its creator (this child) was inside a job when it did.
    pub fn started(&self, launch_id: &str, pid: u32, breakaway: bool) {
        if !lock(&self.state).slots.contains_key(launch_id) {
            return;
        }
        let started_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        self.channel
            .send(ToolChildMessage::Child(ChildToServer::CommandStarted {
                launch_id: launch_id.to_string(),
                pid: i64::from(pid),
                started_at_ms,
                creator_in_job: (self.creator_in_job)(),
                breakaway,
            }));
    }

    /// Test-only `SKIP_ACK_WAIT` mutation: launch paths invoke without awaiting the ack.
    pub fn skips_ack_wait(&self) -> bool {
        self.controls.enabled(TestSwitch::SkipAckWait)
    }

    pub fn request(&self, request: LaunchRequest) -> Result<GatedLaunch, LaunchGateError> {
        let slot = self.clock.claim_slot().ok_or_else(|| {
            LaunchGateError::new(
                "tool_child_launch_capacity",
                format!("{} launches in flight", self.clock.slots),
            )
        })?;
        let (ack, acked) = oneshot::channel();
        let launch_id = {
            let mut state = lock(&self.state);
            state.next += 1;
            let launch_id = format!("L{}-{}", std::process::id(), state.next);
            state.slots.insert(
                launch_id.clone(),
                SlotEntry {
                    slot,
                    ack: Some(ack),
                    lifetime: request.lifetime,
                    request: request.clone(),
                },
            );
            launch_id
        };
        self.channel.send(launch_message(&launch_id, &request));
        #[cfg(unix)]
        if self.controls.enabled(TestSwitch::StopAfterSpawn) {
            let _ = rustix::process::kill_process(
                rustix::process::getpid(),
                rustix::process::Signal::STOP,
            );
        }
        Ok(GatedLaunch {
            launch_id,
            slot,
            acked,
        })
    }

    /// Protocol loop: a launch a worker announced on a slot it claimed itself on the shared clock.
    pub fn adopt(&self, launch_id: &str, slot: usize, request: LaunchRequest) {
        lock(&self.state).slots.insert(
            launch_id.to_string(),
            SlotEntry {
                slot,
                ack: None,
                lifetime: request.lifetime,
                request: request.clone(),
            },
        );
        self.channel.send(launch_message(launch_id, &request));
    }

    pub fn handle_worker(&self, message: WorkerLaunchMessage) {
        match message {
            WorkerLaunchMessage::Launch {
                launch_id,
                slot,
                request,
            } => self.adopt(&launch_id, slot, request),
            WorkerLaunchMessage::Invoking { launch_id } => self.invoking(&launch_id),
            WorkerLaunchMessage::Started {
                launch_id,
                pid,
                breakaway,
            } => self.started(&launch_id, pid, breakaway),
            WorkerLaunchMessage::Done {
                launch_id,
                exit_code,
                signal,
            } => self.done(&launch_id, exit_code, signal),
        }
    }

    /// Protocol loop: a frame the gate owns. Returns false for frames it does not handle.
    pub fn handle(&self, message: &ToolChildMessage) -> bool {
        match message {
            ToolChildMessage::Server(ServerToChild::LaunchAck { launch_id }) => {
                let ack = {
                    let mut state = lock(&self.state);
                    state
                        .slots
                        .get_mut(launch_id)
                        .map(|entry| (entry.slot, entry.ack.take()))
                };
                if let Some((slot, ack)) = ack {
                    let _ = self.clock.record_ack(slot);
                    if let Some(ack) = ack {
                        let _ = ack.send(());
                    }
                }
                true
            }
            ToolChildMessage::Server(ServerToChild::SlotQuery { launch_id }) => {
                let state = {
                    let state = lock(&self.state);
                    match state.slots.get(launch_id) {
                        Some(entry) => self.clock.read(entry.slot).unwrap_or(SlotState {
                            ack_tick: None,
                            invoke_tick: None,
                            invocations: 0,
                        }),
                        None => state.finished.get(launch_id).cloned().unwrap_or(SlotState {
                            ack_tick: None,
                            invoke_tick: None,
                            invocations: 0,
                        }),
                    }
                };
                self.channel
                    .send(ToolChildMessage::Child(ChildToServer::SlotState {
                        launch_id: launch_id.clone(),
                        ack_tick: state.ack_tick,
                        invoke_tick: state.invoke_tick,
                        invocations: state.invocations,
                    }));
                true
            }
            _ => false,
        }
    }

    /// Main-thread launch path: records the invoke tick and publishes the proof. Call immediately before the real call.
    pub fn invoke_now(&self, launch_id: &str) -> Result<(), LaunchGateError> {
        let slot = lock(&self.state)
            .slots
            .get(launch_id)
            .map(|entry| entry.slot);
        let Some(slot) = slot else {
            return Err(LaunchGateError::new("tool_child_launch_unknown", launch_id));
        };
        let _ = self.clock.record_invoke(slot);
        self.invoking(launch_id);
        Ok(())
    }

    /// Protocol loop: a launch path (any thread) has stored its invoke tick; publish the proof from the clock.
    /// A held proof (test-only `HOLD_LAUNCH_PROOF`) is released from a task, so this must run inside a tokio runtime.
    pub fn invoking(&self, launch_id: &str) {
        let (slot, lifetime) = {
            let state = lock(&self.state);
            match state.slots.get(launch_id) {
                Some(entry) => (entry.slot, entry.lifetime),
                None => return,
            }
        };
        let Ok(state) = self.clock.read(slot) else {
            return;
        };
        let Some(invoke_tick) = state.invoke_tick else {
            return;
        };
        let message = ToolChildMessage::Child(ChildToServer::LaunchProof {
            launch_id: launch_id.to_string(),
            ack_tick: state.ack_tick,
            invoke_tick,
        });
        let hold = self.controls.value(TestSwitch::HoldLaunchProof);
        if let Some(hold) = hold
            && (hold != "detached" || lifetime == LaunchLifetime::Detached)
        {
            let controls = Arc::clone(&self.controls);
            let channel = Arc::clone(&self.channel);
            let file = format!("release-proof-{launch_id}");
            tokio::spawn(async move {
                if controls.released(&file).await.is_ok() {
                    channel.send(message);
                }
            });
            return;
        }
        self.channel.send(message);
    }

    /// Ends the owned own-group commands a cancelled job started that are still running (no `launch_done` yet). Their
    /// leaders are this process's unreaped children, so each pgid is still the group this job created - never a reused
    /// one. Detached launches outlive their job by contract and are left alone. Returns the groups signalled.
    pub fn end_job_groups(&self, job_id: &str) -> Vec<u32> {
        let candidates: Vec<(String, u32)> = lock(&self.state)
            .slots
            .iter()
            .filter(|(_, entry)| {
                let request = &entry.request;
                request.job_id == job_id
                    && request.lifetime == LaunchLifetime::Owned
                    && request.containment == LaunchContainment::OwnGroup
            })
            .filter_map(|(launch_id, entry)| match entry.request.pgid {
                Some(pgid) if pgid > 1 => Some((launch_id.clone(), pgid)),
                _ => None,
            })
            .collect();
        let ended: Vec<(String, u32)> = candidates
            .into_iter()
            .filter(|(_, pgid)| kill_group(*pgid))
            .collect();
        // The worker that would have reported these launches' completion is gone with its job: report it here, so the
        // slots return to the clock and the server's log is complete.
        for (launch_id, _) in &ended {
            self.done(launch_id, None, Some("SIGKILL".to_string()));
        }
        ended.into_iter().map(|(_, pgid)| pgid).collect()
    }

    pub fn done(&self, launch_id: &str, exit_code: Option<i32>, signal: Option<String>) {
        let slot = {
            let mut state = lock(&self.state);
            let Some(entry) = state.slots.remove(launch_id) else {
                return;
            };
            if let Ok(read) = self.clock.read(entry.slot) {
                state.finished.insert(launch_id.to_string(), read);
            }
            state.finished_order.push_back(launch_id.to_string());
            if state.finished.len() > FINISHED_SLOT_STATES
                && let Some(oldest) = state.finished_order.pop_front()
            {
                state.finished.remove(&oldest);
            }
            entry.slot
        };
        let _ = self.clock.release_slot(slot);
        self.channel
            .send(ToolChildMessage::Child(ChildToServer::LaunchDone {
                launch_id: launch_id.to_string(),
                exit_code: exit_code.map(i64::from),
                signal,
            }));
    }
}

/// `process.kill(-pgid, "SIGKILL")`; false when the group ended between its last member and our signal.
#[cfg(unix)]
fn kill_group(pgid: u32) -> bool {
    let Some(pid) = i32::try_from(pgid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return false;
    };
    rustix::process::kill_process_group(pid, rustix::process::Signal::KILL).is_ok()
}

#[cfg(not(unix))]
fn kill_group(_pgid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_gate_error_carries_its_code_in_the_message() {
        let error = LaunchGateError::new("tool_child_launch_unknown", "L1-1");
        assert_eq!(error.to_string(), "tool_child_launch_unknown: L1-1");
        assert_eq!(error.code, "tool_child_launch_unknown");
    }

    #[test]
    fn finished_slot_state_cap_matches_upstream() {
        assert_eq!(FINISHED_SLOT_STATES, 256);
    }

    #[cfg(unix)]
    #[test]
    fn a_group_that_does_not_exist_is_not_reported_ended() {
        // pgid 1 is never signalled by `end_job_groups`; an out-of-range id cannot be a pid at all.
        assert!(!kill_group(u32::MAX));
    }
}
