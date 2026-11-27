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

    fn ask(&self, lane: &str, generation: u64) {
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
        } else {
            let mut guard = self.lanes.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(st) = guard.get_mut(lane)
                && st.generation == generation
                && st.verdict.is_none()
            {
                Self::decide(st, Some("hand_unreachable: no runtime".to_owned()));
            }
        }
    }

    pub async fn open(&self, lane: &str) -> Option<String> {
        let (notify, generation) = {
            let mut guard = self.lanes.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(known) = guard.get(lane) {
                if let Some(ref verdict) = known.verdict {
                    return verdict.clone();
                }
                (Arc::clone(&known.notify), None)
            } else {
                let notify = Arc::new(Notify::new());
                let state = LaneState {
                    generation: 1,
                    verdict: None,
                    notify: Arc::clone(&notify),
                };
                guard.insert(lane.to_owned(), state);
                (notify, Some(1))
            }
        };
        if let Some(generation) = generation {
            self.ask(lane, generation);
        }

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
        let lanes: Vec<(String, u64)> = {
            let mut guard = self.lanes.lock().unwrap_or_else(|p| p.into_inner());
            guard
                .iter_mut()
                .filter(|(_, state)| matches!(state.verdict, None | Some(None)))
                .map(|(lane, state)| {
                    state.verdict = None;
                    state.generation += 1;
                    (lane.clone(), state.generation)
                })
                .collect()
        };
        for (lane, generation) in lanes {
            self.ask(&lane, generation);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn floor_sender_runs_without_the_lane_lock() {
        let slot = Arc::new(Mutex::new(None::<std::sync::Weak<EpochGate>>));
        let callback_slot = slot.clone();
        let (sent, mut observed) = tokio::sync::mpsc::unbounded_channel();
        let gate = create_epoch_gate(
            7,
            Arc::new(move |_, _| {
                let gate = callback_slot
                    .lock()
                    .expect("gate slot")
                    .as_ref()
                    .expect("gate bound")
                    .upgrade()
                    .expect("gate alive");
                sent.send(gate.lanes.try_lock().is_ok())
                    .expect("test observes send");
                Box::pin(async { Ok(()) })
            }),
        );
        *slot.lock().expect("gate slot") = Some(Arc::downgrade(&gate));
        let opened_gate = gate.clone();
        let opened = tokio::spawn(async move { opened_gate.open("lane").await });
        let unlocked = tokio::time::timeout(std::time::Duration::from_secs(5), observed.recv())
            .await
            .expect("sender is called")
            .expect("sender remains open");
        assert!(unlocked, "floor sender ran under lane lock");
        gate.answer("lane", FloorReply::Ack { floor: 7 });
        assert_eq!(opened.await.expect("open task"), None);
        gate.resend();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), observed.recv())
                .await
                .expect("resend completes")
                .expect("resend invokes sender"),
            "resend invoked sender under lane lock"
        );
    }
}
