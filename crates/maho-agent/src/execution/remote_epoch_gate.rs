//! Port of senpi `packages/agent/src/execution/remote-epoch-gate.ts`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use maho_ai::types::BoxFuture;
use tokio::sync::Notify;

use super::remote_protocol::EpochRefusal;

pub enum FloorReply {
    Ack { floor: u64 },
    Refused { reason: EpochRefusal },
}

pub type SendFloorFn =
    Arc<dyn Fn(&str, u64) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

struct LaneState {
    generation: u64,
    verdict: Option<Option<String>>,
    notify: Arc<Notify>,
}

pub struct EpochGate {
    epoch: u64,
    send: SendFloorFn,
    lanes: Arc<Mutex<HashMap<String, LaneState>>>,
}

impl EpochGate {
    fn decide(state: &mut LaneState, refusal: Option<String>) {
        state.verdict = Some(refusal);
        state.notify.notify_waiters();
    }

    fn ask(&self, lane: &str, state: &mut LaneState) {
        if state.verdict.is_some() {
            state.verdict = None;
        }
        state.generation += 1;
        let generation = state.generation;
        let send_fut = (self.send)(lane, self.epoch);
        let lanes = Arc::clone(&self.lanes);
        let lane_owned = lane.to_owned();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                if let Err(err) = send_fut.await {
                    let mut guard = lanes.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(st) = guard.get_mut(&lane_owned)
                        && st.generation == generation
                        && st.verdict.is_none()
                    {
                        Self::decide(st, Some(format!("hand_unreachable: {err}")));
                    }
                }
            });
        }
    }

    pub async fn open(&self, lane: &str) -> Option<String> {
        let notify = {
            let mut guard = self.lanes.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(known) = guard.get(lane) {
                if let Some(ref verdict) = known.verdict {
                    return verdict.clone();
                }
                Arc::clone(&known.notify)
            } else {
                let notify = Arc::new(Notify::new());
                let mut state = LaneState {
                    generation: 0,
                    verdict: None,
                    notify: Arc::clone(&notify),
                };
                self.ask(lane, &mut state);
                guard.insert(lane.to_owned(), state);
                notify
            }
        };

        loop {
            let notified = notify.notified();
            if let Some(ref verdict) = self
                .lanes
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(lane)
                .and_then(|s| s.verdict.clone())
            {
                return verdict.clone();
            }
            notified.await;
        }
    }

    pub fn answer(&self, lane: &str, message: FloorReply) {
        let mut guard = self.lanes.lock().unwrap_or_else(|p| p.into_inner());
        let Some(state) = guard.get_mut(lane) else {
            return;
        };
        if state.verdict.is_some() {
            return;
        }
        match message {
            FloorReply::Refused { reason } => {
                Self::decide(state, Some(reason.as_str().to_owned()));
            }
            FloorReply::Ack { floor } => {
                if floor == self.epoch {
                    Self::decide(state, None);
                } else {
                    let msg = if floor > self.epoch {
                        "epoch_superseded"
                    } else {
                        "epoch_floor_mismatch"
                    };
                    Self::decide(state, Some(msg.to_owned()));
                }
            }
        }
    }

    pub fn resend(&self) {
        let mut guard = self.lanes.lock().unwrap_or_else(|p| p.into_inner());
        let lanes: Vec<String> = guard
            .iter()
            .filter(|(_, s)| matches!(s.verdict, None | Some(None)))
            .map(|(l, _)| l.clone())
            .collect();
        for lane in lanes {
            if let Some(state) = guard.get_mut(&lane) {
                self.ask(&lane, state);
            }
        }
    }
}

pub fn create_epoch_gate(epoch: u64, send: SendFloorFn) -> Arc<EpochGate> {
    Arc::new(EpochGate {
        epoch,
        send,
        lanes: Arc::new(Mutex::new(HashMap::new())),
    })
}
