//! Port of senpi `packages/agent/src/execution/loop-hook-host.ts`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use maho_ai::types::BoxFuture;
use maho_ai::utils::abort::AbortSignal;
use serde_json::Value;
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::transport::transport::{FrameHandler, Transport, TransportFrame};

use super::loop_hook_host_protocol::{
    LoopHookMessage, decode_loop_hook_message, encode_loop_hook_message,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoopHookError {
    #[error("{0}")]
    Failed(String),
    #[error(
        "The user's machine restarted while it ran the {event} hooks; they were not run again and may have run."
    )]
    Restarted { event: String },
    #[error("{0}")]
    Other(String),
}

impl LoopHookError {
    pub fn uncertain(&self) -> bool {
        matches!(self, Self::Restarted { .. })
    }
}

pub struct LoopHookHostOptions {
    pub transport: Arc<dyn Transport>,
    pub lane: String,
    pub mint_id: Option<Arc<dyn Fn() -> String + Send + Sync>>,
}

struct PendingHookCall {
    call_id: String,
    event: String,
    payload: Value,
    started_by: Option<String>,
    tx: oneshot::Sender<Result<Value, LoopHookError>>,
}

pub struct LoopHookHost {
    transport: Arc<dyn Transport>,
    lane: String,
    mint_id: Arc<dyn Fn() -> String + Send + Sync>,
    pending: Arc<Mutex<HashMap<String, PendingHookCall>>>,
    closed: Arc<AtomicBool>,
}

impl LoopHookHost {
    fn settle(
        pending: &Arc<Mutex<HashMap<String, PendingHookCall>>>,
        call_id: &str,
        result: Result<Value, LoopHookError>,
    ) {
        let waiter = pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(call_id);
        if let Some(waiter) = waiter {
            let _ = waiter.tx.send(result);
        }
    }

    fn send_call(&self, call_id: String, event: String, payload: Value) {
        let transport = Arc::clone(&self.transport);
        let lane = self.lane.clone();
        let pending = Arc::clone(&self.pending);
        let bytes = encode_loop_hook_message(&LoopHookMessage::Call {
            call_id: call_id.clone(),
            event,
            payload,
        });
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                if let Err(err) = transport.send(&lane, bytes).await {
                    Self::settle(
                        &pending,
                        &call_id,
                        Err(LoopHookError::Other(format!(
                            "The link to the user's machine closed: {err}"
                        ))),
                    );
                }
            });
        }
    }

    pub fn run(
        &self,
        event: &str,
        payload: Value,
        signal: Option<AbortSignal>,
    ) -> BoxFuture<'_, Result<Value, LoopHookError>> {
        let event = event.to_owned();
        Box::pin(async move {
            if self.closed.load(Ordering::SeqCst) {
                return Err(LoopHookError::Other(
                    "The loop-hook link is closed".to_owned(),
                ));
            }
            if signal.as_ref().is_some_and(AbortSignal::aborted) {
                return Err(LoopHookError::Other("Operation aborted".to_owned()));
            }
            let call_id = (self.mint_id)();
            let (tx, mut rx) = oneshot::channel();
            self.pending
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(
                    call_id.clone(),
                    PendingHookCall {
                        call_id: call_id.clone(),
                        event: event.clone(),
                        payload: payload.clone(),
                        started_by: None,
                        tx,
                    },
                );
            self.send_call(call_id.clone(), event, payload);

            tokio::select! {
                res = &mut rx => {
                    match res {
                        Ok(val) => val,
                        Err(_) => Err(LoopHookError::Other("Operation aborted".to_owned())),
                    }
                }
                _ = async {
                    match &signal {
                        Some(sig) => sig.cancelled().await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    Self::settle(&self.pending, &call_id, Err(LoopHookError::Other("Operation aborted".to_owned())));
                    Err(LoopHookError::Other("Operation aborted".to_owned()))
                }
            }
        })
    }

    pub fn waiting(&self) -> Vec<String> {
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .cloned()
            .collect()
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let keys: Vec<String> = self
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .cloned()
            .collect();
        for call_id in keys {
            Self::settle(
                &self.pending,
                &call_id,
                Err(LoopHookError::Other(
                    "The loop-hook link was closed before the hooks ran".to_owned(),
                )),
            );
        }
        self.transport.close();
    }
}

pub fn create_loop_hook_host(options: LoopHookHostOptions) -> Arc<LoopHookHost> {
    let pending: Arc<Mutex<HashMap<String, PendingHookCall>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let host = Arc::new(LoopHookHost {
        transport: Arc::clone(&options.transport),
        lane: options.lane.clone(),
        mint_id: options
            .mint_id
            .unwrap_or_else(|| Arc::new(|| Uuid::new_v4().to_string())),
        pending: Arc::clone(&pending),
        closed: Arc::new(AtomicBool::new(false)),
    });

    let pending_frame = Arc::clone(&pending);
    let transport_frame = Arc::clone(&options.transport);
    let lane_frame = options.lane;
    let handler: FrameHandler = Arc::new(move |frame: TransportFrame| {
        let pending = Arc::clone(&pending_frame);
        let transport = Arc::clone(&transport_frame);
        let lane = lane_frame.clone();
        Box::pin(async move {
            let Some(message) = decode_loop_hook_message(&frame.payload) else {
                return;
            };
            match message {
                LoopHookMessage::Started { call_id, instance } => {
                    if let Some(waiter) = pending
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .get_mut(&call_id)
                    {
                        waiter.started_by = Some(instance);
                    }
                }
                LoopHookMessage::Reply { call_id, result } => {
                    LoopHookHost::settle(&pending, &call_id, Ok(result));
                }
                LoopHookMessage::Failed { call_id, error } => {
                    LoopHookHost::settle(&pending, &call_id, Err(LoopHookError::Failed(error)));
                }
                LoopHookMessage::Hello { instance } => {
                    let mut to_resend = Vec::new();
                    let mut to_fail = Vec::new();
                    {
                        let guard = pending.lock().unwrap_or_else(|p| p.into_inner());
                        for waiter in guard.values() {
                            match &waiter.started_by {
                                None => to_resend.push((
                                    waiter.call_id.clone(),
                                    waiter.event.clone(),
                                    waiter.payload.clone(),
                                )),
                                Some(prev) if prev != &instance => {
                                    to_fail.push((waiter.call_id.clone(), waiter.event.clone()));
                                }
                                Some(_) => {}
                            }
                        }
                    }
                    for (call_id, event) in to_fail {
                        LoopHookHost::settle(
                            &pending,
                            &call_id,
                            Err(LoopHookError::Restarted { event }),
                        );
                    }
                    for (call_id, event, payload) in to_resend {
                        let bytes = encode_loop_hook_message(&LoopHookMessage::Call {
                            call_id: call_id.clone(),
                            event,
                            payload,
                        });
                        if let Err(err) = transport.send(&lane, bytes).await {
                            LoopHookHost::settle(
                                &pending,
                                &call_id,
                                Err(LoopHookError::Other(format!(
                                    "The link to the user's machine closed: {err}"
                                ))),
                            );
                        }
                    }
                }
                LoopHookMessage::Call { .. } => {}
            }
        })
    });

    options.transport.on_frame(handler);
    host
}
