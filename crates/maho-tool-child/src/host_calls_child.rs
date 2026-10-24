//! senpi `modes/rpc/tool-child/host-calls-child.ts` (pin 26acd9353).
//!
//! The child side of the reverse bridge (senpi#3566 todo 7): a job's call to the server. Every call is admitted against
//! its job's host-call capacity before it is sent - running, queued FIFO and sent when a slot frees, or refused with a
//! correlated error. Nothing is dropped: each call settles exactly once, by its reply, its refusal, its job's abort or the
//! channel closing.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use maho_ext_api::AbortSignal;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::admission::{HostCallAdmission, HostCallDecision};
use crate::entry_error::ToolChildJobError;
use crate::host_calls_server::host_call_kind_name;
use crate::host_calls_server::{ChannelSend, HostCallRefs};
use crate::payload_wire::{PackedPayload, pack_payload, unpack_payload};
use crate::protocol::{
    ChildToServer, HostCallKind, PayloadRef, RefControl, RefNackReason, ServerToChild,
    ToolChildMessage,
};

#[derive(Clone, Debug)]
pub struct HostCallTarget {
    pub kind: HostCallKind,
    pub name: Option<String>,
    pub args: Option<Value>,
}

type CallResult = Result<Value, ToolChildJobError>;

/// The fields of the `host_call` frame a pending call keeps.
#[derive(Clone)]
struct HostCallFrame {
    call_id: String,
    job_id: String,
    kind: HostCallKind,
    name: Option<String>,
    args: Option<Value>,
    args_ref: Option<PayloadRef>,
}

impl HostCallFrame {
    fn message(&self) -> ToolChildMessage {
        ToolChildMessage::Child(ChildToServer::HostCall {
            call_id: self.call_id.clone(),
            job_id: self.job_id.clone(),
            kind: self.kind,
            name: self.name.clone(),
            args: self.args.clone(),
            args_ref: self.args_ref.clone(),
        })
    }
}

struct PendingCall {
    frame: HostCallFrame,
    tx: oneshot::Sender<CallResult>,
    /// On the wire: from here its ref (if any) is the server's to ack or nack.
    sent: bool,
}

#[derive(Default)]
struct State {
    admissions: HashMap<String, HostCallAdmission>,
    pending: HashMap<String, PendingCall>,
    next: u64,
}

impl State {
    fn admission(&mut self, job_id: &str) -> &mut HostCallAdmission {
        self.admissions.entry(job_id.to_owned()).or_default()
    }

    fn forget_idle(&mut self, job_id: &str) {
        if let Some(admission) = self.admissions.get(job_id)
            && admission.active() == 0
            && admission.waiting() == 0
        {
            self.admissions.remove(job_id);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct ChildHostCalls {
    channel: Arc<dyn ChannelSend>,
    refs: Option<HostCallRefs>,
    state: Arc<Mutex<State>>,
}

impl ChildHostCalls {
    pub fn new(channel: Arc<dyn ChannelSend>, refs: Option<HostCallRefs>) -> Self {
        Self {
            channel,
            refs,
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    /// Calls running and queued for one job (0 for a job with none): `(active, waiting)`.
    pub fn load(&self, job_id: &str) -> (usize, usize) {
        let state = lock(&self.state);
        match state.admissions.get(job_id) {
            Some(admission) => (admission.active(), admission.waiting()),
            None => (0, 0),
        }
    }

    pub async fn call(
        &self,
        job_id: &str,
        target: HostCallTarget,
        signal: &AbortSignal,
    ) -> Result<Value, ToolChildJobError> {
        let label = target
            .name
            .clone()
            .unwrap_or_else(|| host_call_kind_name(target.kind).to_owned());
        if signal.is_aborted() {
            return Err(cancelled(&label));
        }
        let call_id = {
            let mut state = lock(&self.state);
            state.next += 1;
            format!("h{}", state.next)
        };
        let packed = match (&target.args, &self.refs) {
            (Some(args), Some(refs)) => pack_payload(args, &refs.writer)
                .await
                .map_err(|error| ToolChildJobError::new("host_call_failed", error.to_string()))?,
            _ => PackedPayload::Inline(target.args.clone().unwrap_or(Value::Null)),
        };
        let (args, args_ref) = match packed {
            PackedPayload::Inline(value) => (target.args.as_ref().map(|_| value), None),
            PackedPayload::Ref(payload_ref) => (None, Some(payload_ref)),
        };
        let frame = HostCallFrame {
            call_id: call_id.clone(),
            job_id: job_id.to_owned(),
            kind: target.kind,
            name: target.name.clone(),
            args,
            args_ref,
        };
        // An abort that landed while the payload was packed is seen here, before anything is sent.
        if signal.is_aborted() {
            self.release_unsent(&frame);
            return Err(cancelled(&label));
        }
        let (tx, mut rx) = oneshot::channel();
        let send_now = {
            let mut state = lock(&self.state);
            match state.admission(job_id).submit(&call_id) {
                HostCallDecision::Refused { frame: refusal } => {
                    state.forget_idle(job_id);
                    drop(state);
                    self.release_unsent(&frame);
                    let code = match *refusal {
                        ToolChildMessage::Server(ServerToChild::HostCallRefused {
                            code, ..
                        }) => code,
                        _ => String::new(),
                    };
                    return Err(ToolChildJobError::new(
                        code,
                        format!("{label}: host call {call_id} refused"),
                    ));
                }
                decision => {
                    state.pending.insert(
                        call_id.clone(),
                        PendingCall {
                            frame,
                            tx,
                            sent: false,
                        },
                    );
                    decision == HostCallDecision::Active
                }
            }
        };
        if send_now {
            self.send(&call_id);
        }
        tokio::select! {
            outcome = &mut rx => return outcome.unwrap_or_else(|_| Err(ToolChildJobError::new("tool_child_exited", "the channel closed"))),
            () = signal.cancelled() => {}
        }
        // The job's abort: the call settles as cancelled unless its reply landed first (then `rx` holds that outcome).
        self.settle(&call_id, |call| {
            if !call.sent {
                self.release_unsent(&call.frame);
            }
            let _ = call.tx.send(Err(cancelled(&label)));
        });
        rx.await.unwrap_or_else(|_| Err(cancelled(&label)))
    }

    /// Routes `host_reply` / `host_call_refused`; returns false for every other frame. Must run inside a tokio runtime.
    pub fn handle(&self, frame: &ToolChildMessage) -> bool {
        match frame {
            ToolChildMessage::Server(ServerToChild::HostReply {
                call_id,
                value,
                value_ref,
                error,
            }) => {
                self.settle(call_id, |call| {
                    if let Some(error) = error {
                        let _ = call.tx.send(Err(ToolChildJobError::new(
                            "host_call_failed",
                            error.clone(),
                        )));
                    } else if let Some(value_ref) = value_ref {
                        match &self.refs {
                            None => {
                                let _ = call.tx.send(Err(ToolChildJobError::new(
                                    "host_call_failed",
                                    "a valueRef reply on a side that reads no refs",
                                )));
                            }
                            Some(refs) => {
                                let reader = refs.reader.clone();
                                let value_ref = value_ref.clone();
                                tokio::spawn(async move {
                                    let outcome = unpack_payload(&value_ref, &reader, None)
                                        .await
                                        .map_err(|error| {
                                            ToolChildJobError::new(
                                                "host_call_failed",
                                                error.to_string(),
                                            )
                                        });
                                    let _ = call.tx.send(outcome);
                                });
                            }
                        }
                    } else {
                        let _ = call.tx.send(Ok(value.clone().unwrap_or(Value::Null)));
                    }
                });
                true
            }
            ToolChildMessage::Server(ServerToChild::HostCallRefused { call_id, code }) => {
                self.settle(call_id, |call| {
                    let _ = call.tx.send(Err(ToolChildJobError::new(
                        code.clone(),
                        format!("host call {call_id} refused by the server"),
                    )));
                });
                true
            }
            _ => false,
        }
    }

    /// The channel closed: every pending call fails, none is left waiting.
    pub fn close(&self) {
        let call_ids: Vec<String> = lock(&self.state).pending.keys().cloned().collect();
        for call_id in call_ids {
            self.settle(&call_id, |call| {
                let _ = call.tx.send(Err(ToolChildJobError::new(
                    "tool_child_exited",
                    "the channel closed",
                )));
            });
        }
    }

    fn send(&self, call_id: &str) {
        let message = {
            let mut state = lock(&self.state);
            let Some(call) = state.pending.get_mut(call_id) else {
                return;
            };
            call.sent = true;
            call.frame.message()
        };
        self.channel.send(message);
    }

    /// A call that never left the child gives its own ref back: the writer deletes it and frees its budget.
    fn release_unsent(&self, frame: &HostCallFrame) {
        if let (Some(args_ref), Some(refs)) = (&frame.args_ref, &self.refs) {
            let writer = refs.writer.clone();
            let control = RefControl::RefNack {
                ref_id: args_ref.ref_id.clone(),
                reason: RefNackReason::Cancelled,
            };
            tokio::spawn(async move {
                let _ = writer.handle(&control).await;
            });
        }
    }

    fn settle(&self, call_id: &str, apply: impl FnOnce(PendingCall)) {
        let (call, promoted) = {
            let mut state = lock(&self.state);
            let Some(call) = state.pending.remove(call_id) else {
                return;
            };
            let job_id = call.frame.job_id.clone();
            let promoted = state
                .admissions
                .get_mut(&job_id)
                .and_then(|admission| admission.settle(call_id));
            state.forget_idle(&job_id);
            (call, promoted)
        };
        apply(call);
        if let Some(promoted) = promoted {
            self.send(&promoted);
        }
    }
}

fn cancelled(label: &str) -> ToolChildJobError {
    ToolChildJobError::new("tool_child_cancelled", format!("{label}: job cancelled"))
}
