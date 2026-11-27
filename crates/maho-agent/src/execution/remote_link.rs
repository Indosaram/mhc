//! Port of senpi `packages/agent/src/execution/remote-link.ts`.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use maho_ai::types::BoxFuture;
use maho_ai::utils::abort::AbortSignal;
use tokio::sync::oneshot;

use crate::transport::transport::{FrameHandler, Transport, TransportFrame};

use super::plane::{
    PlaneHandler, PlaneServer, PlaneServerOptions, create_plane_server, decode_plane_message,
};
use super::remote_epoch_gate::{EpochGate, FloorReply, create_epoch_gate};
use super::remote_protocol::{
    RemoteFile, RemoteHandMessage, RemoteHello, RemoteInvoke, RemoteTerminal, RemoteToolResult,
    decode_remote_message, encode_remote_message,
};

#[derive(Debug, Clone, PartialEq)]
pub enum HandReply {
    Settled { result: RemoteToolResult },
    Terminal { terminal: RemoteTerminal },
    Refused { reason: String },
    Aborted { sent: bool },
    Unreachable { reason: String },
    Closed { sent: bool },
    EpochRefused { reason: String },
}

#[derive(Clone)]
enum DispatchMessage {
    Invoke(RemoteInvoke),
    File(RemoteFile),
}

impl DispatchMessage {
    fn execution_id(&self) -> &str {
        match self {
            Self::Invoke(m) => &m.execution_id,
            Self::File(m) => &m.execution_id,
        }
    }

    fn scope_id(&self) -> &str {
        match self {
            Self::Invoke(m) => &m.scope.id,
            Self::File(m) => &m.scope.id,
        }
    }

    fn with_epoch(self, epoch: Option<u64>) -> Self {
        let Some(ep) = epoch else {
            return self;
        };
        match self {
            Self::Invoke(mut m) => {
                m.epoch = Some(ep);
                Self::Invoke(m)
            }
            Self::File(mut m) => {
                m.epoch = Some(ep);
                Self::File(m)
            }
        }
    }

    fn into_wire(self) -> RemoteHandMessage {
        match self {
            Self::Invoke(m) => RemoteHandMessage::Invoke(m),
            Self::File(m) => RemoteHandMessage::File(m),
        }
    }
}

struct PendingEntry {
    message: DispatchMessage,
    tx: oneshot::Sender<HandReply>,
}

struct HelloWaiter {
    tx: oneshot::Sender<Option<RemoteHello>>,
}

struct LinkInner {
    transport: Arc<dyn Transport>,
    epoch: Option<u64>,
    pending: Mutex<HashMap<String, PendingEntry>>,
    closed: AtomicBool,
    sent: Mutex<HashSet<String>>,
    planes: Arc<PlaneServer>,
    gate: Option<Arc<EpochGate>>,
    announced: Mutex<Option<RemoteHello>>,
    waited_out: AtomicBool,
    hello_waiters: Mutex<Vec<HelloWaiter>>,
}

impl LinkInner {
    fn settle(&self, execution_id: &str, reply: HandReply) {
        let waiter = self
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(execution_id);
        let Some(waiter) = waiter else {
            return;
        };
        self.sent
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(execution_id);
        self.planes.parent_ended(execution_id);
        let _ = waiter.tx.send(reply);
    }

    fn has_pending(&self, execution_id: &str) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(execution_id)
    }

    fn when_open<F>(self: &Arc<Self>, lane: String, execution_id: String, go: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let Some(gate) = self.gate.as_ref().map(Arc::clone) else {
            go();
            return;
        };
        let inner = Arc::clone(self);
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let refusal = gate.open(&lane).await;
                if !inner.has_pending(&execution_id) {
                    return;
                }
                match refusal {
                    None => go(),
                    Some(reason) => inner.settle(&execution_id, HandReply::EpochRefused { reason }),
                }
            });
        }
    }

    fn dispatch_msg(
        self: &Arc<Self>,
        msg: DispatchMessage,
        signal: Option<AbortSignal>,
    ) -> BoxFuture<'static, HandReply> {
        let msg = msg.with_epoch(self.epoch);
        let execution_id = msg.execution_id().to_owned();
        let scope_id = msg.scope_id().to_owned();
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(
                execution_id.clone(),
                PendingEntry {
                    message: msg.clone(),
                    tx,
                },
            );

        if self.closed.load(Ordering::SeqCst) {
            self.settle(&execution_id, HandReply::Closed { sent: false });
        } else if signal.as_ref().is_some_and(AbortSignal::aborted) {
            self.settle(&execution_id, HandReply::Aborted { sent: false });
        } else {
            let inner_send = Arc::clone(self);
            let exec_send = execution_id.clone();
            let scope_send = scope_id.clone();
            let scope_abort = scope_id.clone();
            let wire_msg = msg.into_wire();
            self.when_open(scope_id, execution_id.clone(), move || {
                inner_send
                    .sent
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(exec_send.clone());
                let transport = Arc::clone(&inner_send.transport);
                let inner_err = Arc::clone(&inner_send);
                if let Ok(rt) = tokio::runtime::Handle::try_current() {
                    rt.spawn(async move {
                        if let Err(err) = transport
                            .send(&scope_send, encode_remote_message(&wire_msg))
                            .await
                        {
                            inner_err.settle(
                                &exec_send,
                                HandReply::Unreachable {
                                    reason: err.to_string(),
                                },
                            );
                        }
                    });
                }
            });

            if let Some(sig) = signal {
                let inner_abort = Arc::clone(self);
                let exec_abort = execution_id.clone();
                let epoch = self.epoch;
                if let Ok(rt) = tokio::runtime::Handle::try_current() {
                    rt.spawn(async move {
                        sig.cancelled().await;
                        if !inner_abort.has_pending(&exec_abort) {
                            return;
                        }
                        let reached = inner_abort
                            .sent
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .contains(&exec_abort);
                        if reached {
                            let bytes = encode_remote_message(&RemoteHandMessage::Abort {
                                execution_id: exec_abort.clone(),
                                epoch,
                            });
                            let _ = inner_abort.transport.send(&scope_abort, bytes).await;
                        }
                        inner_abort.settle(&exec_abort, HandReply::Aborted { sent: reached });
                    });
                }
            }
        }

        Box::pin(async move { rx.await.unwrap_or(HandReply::Closed { sent: false }) })
    }
}

pub struct HandLink {
    inner: Arc<LinkInner>,
}

impl HandLink {
    pub fn dispatch_invoke(
        &self,
        message: RemoteInvoke,
        signal: Option<AbortSignal>,
    ) -> BoxFuture<'static, HandReply> {
        self.inner
            .dispatch_msg(DispatchMessage::Invoke(message), signal)
    }

    pub fn dispatch_file(
        &self,
        message: RemoteFile,
        signal: Option<AbortSignal>,
    ) -> BoxFuture<'static, HandReply> {
        self.inner
            .dispatch_msg(DispatchMessage::File(message), signal)
    }

    pub fn hello(
        &self,
        ms: u64,
        signal: Option<AbortSignal>,
    ) -> BoxFuture<'_, Option<RemoteHello>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            if let Some(hello) = inner
                .announced
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
            {
                return Some(hello);
            }
            if inner.waited_out.load(Ordering::SeqCst)
                || signal.as_ref().is_some_and(AbortSignal::aborted)
            {
                return None;
            }
            let (tx, rx) = oneshot::channel();
            inner
                .hello_waiters
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(HelloWaiter { tx });
            if let Some(hello) = inner
                .announced
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
            {
                return Some(hello);
            }

            tokio::select! {
                res = rx => res.ok().flatten(),
                _ = tokio::time::sleep(Duration::from_millis(ms)) => {
                    inner.waited_out.store(true, Ordering::SeqCst);
                    None
                }
                _ = async {
                    match &signal {
                        Some(s) => s.cancelled().await,
                        None => std::future::pending::<()>().await,
                    }
                } => None,
            }
        })
    }

    pub fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        let ids: Vec<String> = self
            .inner
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .cloned()
            .collect();
        for execution_id in ids {
            let was_sent = self
                .inner
                .sent
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .contains(&execution_id);
            self.inner
                .settle(&execution_id, HandReply::Closed { sent: was_sent });
        }
        self.inner.transport.close();
    }
}

pub fn create_hand_link(
    transport: Arc<dyn Transport>,
    plane_handlers: HashMap<String, PlaneHandler>,
    file_planes: Vec<String>,
    epoch: Option<u64>,
) -> Arc<HandLink> {
    let file_planes_set: Arc<HashSet<String>> = Arc::new(file_planes.into_iter().collect());
    let pending: Mutex<HashMap<String, PendingEntry>> = Mutex::new(HashMap::new());

    // Build inner via OnceLock / Arc::new_cyclic so `live_lane` inside PlaneServer accesses `pending`.
    let inner = Arc::new_cyclic(|weak: &std::sync::Weak<LinkInner>| {
        let weak_live = weak.clone();
        let file_planes_live = Arc::clone(&file_planes_set);
        let planes = create_plane_server(PlaneServerOptions {
            transport: Arc::clone(&transport),
            planes: plane_handlers,
            live_lane: Arc::new(move |execution_id, plane| {
                let inner = weak_live.upgrade()?;
                let guard = inner.pending.lock().unwrap_or_else(|p| p.into_inner());
                let entry = guard.get(execution_id)?;
                match &entry.message {
                    DispatchMessage::Invoke(m) => Some(m.scope.id.clone()),
                    DispatchMessage::File(m)
                        if plane.is_none_or(|p| file_planes_live.contains(p)) =>
                    {
                        Some(m.scope.id.clone())
                    }
                    _ => None,
                }
            }),
        });

        let gate = epoch.map(|ep| {
            let t = Arc::clone(&transport);
            create_epoch_gate(
                ep,
                Arc::new(move |lane, floor_epoch| {
                    let t = Arc::clone(&t);
                    let lane = lane.to_owned();
                    let bytes =
                        encode_remote_message(&RemoteHandMessage::Floor { epoch: floor_epoch });
                    Box::pin(async move { t.send(&lane, bytes).await.map_err(|e| e.to_string()) })
                }),
            )
        });

        LinkInner {
            transport: Arc::clone(&transport),
            epoch,
            pending,
            closed: AtomicBool::new(false),
            sent: Mutex::new(HashSet::new()),
            planes,
            gate,
            announced: Mutex::new(None),
            waited_out: AtomicBool::new(false),
            hello_waiters: Mutex::new(Vec::new()),
        }
    });

    let inner_frame = Arc::clone(&inner);
    let handler: FrameHandler = Arc::new(move |frame: TransportFrame| {
        let inner = Arc::clone(&inner_frame);
        Box::pin(async move {
            if inner.closed.load(Ordering::SeqCst) {
                return;
            }
            if let Some(nested) = decode_plane_message(&frame.payload) {
                inner.planes.receive(&frame.lane, nested);
                return;
            }
            let Ok(message) = decode_remote_message(&frame.payload) else {
                return;
            };
            let ack = |execution_id: String| {
                let transport = Arc::clone(&inner.transport);
                let lane = frame.lane.clone();
                let bytes = encode_remote_message(&RemoteHandMessage::Ack {
                    execution_id,
                    epoch: inner.epoch,
                });
                if let Ok(rt) = tokio::runtime::Handle::try_current() {
                    rt.spawn(async move {
                        let _ = transport.send(&lane, bytes).await;
                    });
                }
            };
            match message {
                RemoteHandMessage::Hello {
                    instance,
                    durable,
                    ops,
                } => {
                    let hello = RemoteHello {
                        instance,
                        durable,
                        ops,
                    };
                    *inner.announced.lock().unwrap_or_else(|p| p.into_inner()) =
                        Some(hello.clone());
                    let waiters: Vec<HelloWaiter> = inner
                        .hello_waiters
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .drain(..)
                        .collect();
                    for w in waiters {
                        let _ = w.tx.send(Some(hello.clone()));
                    }
                    if let Some(gate) = &inner.gate {
                        gate.resend();
                    }
                    if durable {
                        let to_resend: Vec<(String, String, DispatchMessage)> = {
                            let sent = inner.sent.lock().unwrap_or_else(|p| p.into_inner());
                            let pending = inner.pending.lock().unwrap_or_else(|p| p.into_inner());
                            pending
                                .values()
                                .filter(|e| sent.contains(e.message.execution_id()))
                                .map(|e| {
                                    (
                                        e.message.scope_id().to_owned(),
                                        e.message.execution_id().to_owned(),
                                        e.message.clone(),
                                    )
                                })
                                .collect()
                        };
                        for (scope_id, exec_id, msg) in to_resend {
                            let transport = Arc::clone(&inner.transport);
                            let lane = scope_id.clone();
                            let bytes = encode_remote_message(&msg.into_wire());
                            inner.when_open(scope_id, exec_id, move || {
                                if let Ok(rt) = tokio::runtime::Handle::try_current() {
                                    rt.spawn(async move {
                                        let _ = transport.send(&lane, bytes).await;
                                    });
                                }
                            });
                        }
                    }
                }
                RemoteHandMessage::FloorAck { floor } => {
                    if let Some(gate) = &inner.gate {
                        gate.answer(&frame.lane, FloorReply::Ack { floor });
                    }
                }
                RemoteHandMessage::FloorRefused { reason } => {
                    if let Some(gate) = &inner.gate {
                        gate.answer(&frame.lane, FloorReply::Refused { reason });
                    }
                }
                RemoteHandMessage::Settled {
                    execution_id,
                    result,
                } => {
                    inner.settle(&execution_id, HandReply::Settled { result });
                    ack(execution_id);
                }
                RemoteHandMessage::Terminal(terminal) => {
                    let execution_id = terminal.execution_id.clone();
                    inner.settle(&execution_id, HandReply::Terminal { terminal });
                    ack(execution_id);
                }
                RemoteHandMessage::Refused {
                    execution_id,
                    reason,
                } => {
                    inner.settle(&execution_id, HandReply::Refused { reason });
                }
                _ => {}
            }
        })
    });

    inner.transport.on_frame(handler);
    Arc::new(HandLink { inner })
}
