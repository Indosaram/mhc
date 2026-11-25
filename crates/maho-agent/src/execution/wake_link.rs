//! Port of senpi `packages/agent/src/execution/wake-link.ts`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use maho_ai::types::BoxFuture;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::durable::store::StoreValue;
use crate::transport::transport::{FrameHandler, Transport, TransportFrame};

use super::wake_inbox::{WakeCheckFn, WakeInbox};
use super::wake_outbox::{WakeOutbox, WakeOutboxError};
use super::wake_types::{
    WAKE_DROPPED_SOURCE, WakeDropReason, WakeEvent, WakeOffer, WakeReceipt, WakeRefusal,
};

pub const HAND_WAKE_LANE: &str = "hand.wake";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeDrop {
    pub scope: String,
    pub reason: WakeDropReason,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WakeLinkMessage {
    Wake {
        wake: WakeEvent,
    },
    #[serde(rename_all = "camelCase")]
    WakeDropped {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        event_id: Option<String>,
        scope: String,
        reason: WakeDropReason,
        count: u64,
    },
    WakeReceipt {
        receipt: WakeReceipt,
    },
    WakeReoffer,
}

pub fn encode_wake_link_message(message: &WakeLinkMessage) -> Vec<u8> {
    serde_json::to_vec(message).unwrap_or_default()
}

fn is_id(v: Option<&StoreValue>) -> Option<&str> {
    v.and_then(StoreValue::as_str).filter(|s| !s.is_empty())
}

fn decode_wake_val(value: Option<&StoreValue>) -> Option<WakeEvent> {
    let obj = value?.as_object()?;
    let event_id = is_id(obj.get("eventId"))?.to_owned();
    let scope = is_id(obj.get("scope"))?.to_owned();
    let source = is_id(obj.get("source"))?.to_owned();
    let payload = obj.get("payload")?.clone();
    Some(WakeEvent {
        event_id,
        scope,
        source,
        payload,
    })
}

fn decode_receipt_val(value: Option<&StoreValue>) -> Option<WakeReceipt> {
    let obj = value?.as_object()?;
    let event_id = is_id(obj.get("eventId"))?.to_owned();
    match obj.get("kind").and_then(StoreValue::as_str)? {
        "ack" => {
            let duplicate = obj.get("duplicate")?.as_bool()?;
            Some(WakeReceipt::Ack {
                event_id,
                duplicate,
            })
        }
        "refused" => {
            let reason = WakeRefusal::parse(obj.get("reason")?.as_str()?)?;
            Some(WakeReceipt::Refused { event_id, reason })
        }
        _ => None,
    }
}

pub fn decode_wake_link_message(payload: &[u8]) -> Option<WakeLinkMessage> {
    let val: StoreValue = serde_json::from_slice(payload).ok()?;
    let obj = val.as_object()?;
    match obj.get("kind")?.as_str()? {
        "wake" => Some(WakeLinkMessage::Wake {
            wake: decode_wake_val(obj.get("wake"))?,
        }),
        "wake_receipt" => Some(WakeLinkMessage::WakeReceipt {
            receipt: decode_receipt_val(obj.get("receipt"))?,
        }),
        "wake_dropped" => {
            let scope = is_id(obj.get("scope"))?.to_owned();
            let reason = WakeDropReason::parse(obj.get("reason")?.as_str()?)?;
            let count = obj.get("count")?.as_u64()?;
            if count == 0 {
                return None;
            }
            let event_id = match obj.get("eventId") {
                None | Some(StoreValue::Null) => None,
                Some(v) => Some(v.as_str().filter(|s| !s.is_empty())?.to_owned()),
            };
            Some(WakeLinkMessage::WakeDropped {
                event_id,
                scope,
                reason,
                count,
            })
        }
        "wake_reoffer" => Some(WakeLinkMessage::WakeReoffer),
        _ => None,
    }
}

fn frame_of(wake: WakeEvent) -> WakeLinkMessage {
    if wake.source == WAKE_DROPPED_SOURCE
        && let Some(obj) = wake.payload.as_object()
        && let Some(reason_str) = obj.get("reason").and_then(StoreValue::as_str)
        && let Some(reason) = WakeDropReason::parse(reason_str)
        && let Some(count) = obj.get("count").and_then(StoreValue::as_u64)
    {
        return WakeLinkMessage::WakeDropped {
            event_id: Some(wake.event_id),
            scope: wake.scope,
            reason,
            count,
        };
    }
    WakeLinkMessage::Wake { wake }
}

pub type FaultCallback = Arc<dyn Fn(String) + Send + Sync>;

pub struct HandWakeLinkOptions {
    pub transport: Arc<dyn Transport>,
    pub lane: Option<String>,
    pub outbox: Arc<dyn WakeOutbox>,
    pub on_fault: FaultCallback,
}

pub struct HandWakeLink {
    transport: Arc<dyn Transport>,
    lane: String,
    outbox: Arc<dyn WakeOutbox>,
    on_fault: FaultCallback,
    closed: Arc<AtomicBool>,
    ready_done: Arc<AtomicBool>,
    ready_notify: Arc<Notify>,
}

impl HandWakeLink {
    pub async fn ready(&self) {
        loop {
            let notified = self.ready_notify.notified();
            if self.ready_done.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    async fn send(&self, message: &WakeLinkMessage) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        if let Err(err) = self
            .transport
            .send(&self.lane, encode_wake_link_message(message))
            .await
        {
            (self.on_fault)(err.to_string());
        }
    }

    async fn reoffer(&self) -> Result<(), String> {
        self.outbox.sweep().await?;
        for wake in self.outbox.due().await? {
            self.send(&frame_of(wake)).await;
        }
        Ok(())
    }

    pub async fn offer(&self, wake: WakeOffer) {
        let scope = wake.scope.clone();
        match self.outbox.offer(wake).await {
            Ok(event) => {
                self.send(&WakeLinkMessage::Wake { wake: event }).await;
            }
            Err(WakeOutboxError::Overflow(_)) => {}
            Err(WakeOutboxError::Other(err)) => {
                (self.on_fault)(err);
                self.send(&WakeLinkMessage::WakeDropped {
                    event_id: None,
                    scope,
                    reason: WakeDropReason::Unrecorded,
                    count: 1,
                })
                .await;
            }
        }
    }

    pub async fn close_scope(&self, scope: &str) {
        if let Err(err) = self.outbox.close_scope(scope).await {
            (self.on_fault)(err);
        }
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

pub fn create_hand_wake_link(options: HandWakeLinkOptions) -> Arc<HandWakeLink> {
    let link = Arc::new(HandWakeLink {
        transport: options.transport,
        lane: options.lane.unwrap_or_else(|| HAND_WAKE_LANE.to_owned()),
        outbox: options.outbox,
        on_fault: options.on_fault,
        closed: Arc::new(AtomicBool::new(false)),
        ready_done: Arc::new(AtomicBool::new(false)),
        ready_notify: Arc::new(Notify::new()),
    });

    let link_for_frame = Arc::clone(&link);
    let handler: FrameHandler = Arc::new(move |frame: TransportFrame| {
        let link = Arc::clone(&link_for_frame);
        Box::pin(async move {
            let Some(message) = decode_wake_link_message(&frame.payload) else {
                (link.on_fault)(format!("an undecodable frame on {} was dropped", link.lane));
                return;
            };
            let res = match message {
                WakeLinkMessage::WakeReceipt { receipt } => link.outbox.receipt(receipt).await,
                WakeLinkMessage::WakeReoffer => link.reoffer().await,
                _ => Ok(()),
            };
            if let Err(err) = res {
                (link.on_fault)(err);
            }
        })
    });
    link.transport.on_frame(handler);

    let link_ready = Arc::clone(&link);
    if let Ok(rt) = tokio::runtime::Handle::try_current() {
        rt.spawn(async move {
            if let Err(err) = link_ready.reoffer().await {
                (link_ready.on_fault)(err);
            }
            link_ready.ready_done.store(true, Ordering::SeqCst);
            link_ready.ready_notify.notify_waiters();
        });
    } else {
        link.ready_done.store(true, Ordering::SeqCst);
        link.ready_notify.notify_waiters();
    }

    link
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("the brain refused wake {wake_id} of scope {scope}: {reason:?}", wake_id = .wake.event_id, scope = .wake.scope)]
pub struct WakeRefusedError {
    pub wake: WakeEvent,
    pub reason: WakeRefusal,
}

pub struct BrainWakeLinkOptions {
    pub transport: Arc<dyn Transport>,
    pub lane: Option<String>,
    pub inbox: Arc<dyn WakeInbox>,
    pub hand: String,
    pub check: Option<WakeCheckFn>,
    pub on_committed: Arc<dyn Fn(WakeEvent) + Send + Sync>,
    pub on_dropped: Arc<dyn Fn(WakeDrop) + Send + Sync>,
    pub on_fault: FaultCallback,
}

pub struct BrainWakeLink {
    transport: Arc<dyn Transport>,
    lane: String,
    closed: Arc<AtomicBool>,
    on_fault: FaultCallback,
}

impl BrainWakeLink {
    pub fn reoffer(&self) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        let transport = Arc::clone(&self.transport);
        let lane = self.lane.clone();
        let on_fault = Arc::clone(&self.on_fault);
        let payload = encode_wake_link_message(&WakeLinkMessage::WakeReoffer);
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                if let Err(err) = transport.send(&lane, payload).await {
                    on_fault(err.to_string());
                }
            });
        }
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

pub fn serve_hand_wakes(options: BrainWakeLinkOptions) -> Arc<BrainWakeLink> {
    let lane = options.lane.unwrap_or_else(|| HAND_WAKE_LANE.to_owned());
    let closed = Arc::new(AtomicBool::new(false));
    let link = Arc::new(BrainWakeLink {
        transport: Arc::clone(&options.transport),
        lane: lane.clone(),
        closed: Arc::clone(&closed),
        on_fault: Arc::clone(&options.on_fault),
    });

    let inbox = options.inbox;
    let hand = options.hand;
    let check = options.check;
    let on_committed = options.on_committed;
    let on_dropped = options.on_dropped;
    let on_fault = options.on_fault;
    let transport = options.transport;

    let handler: FrameHandler = Arc::new(move |frame: TransportFrame| {
        let closed = Arc::clone(&closed);
        let lane = lane.clone();
        let inbox = Arc::clone(&inbox);
        let hand = hand.clone();
        let check = check.clone();
        let on_committed = Arc::clone(&on_committed);
        let on_dropped = Arc::clone(&on_dropped);
        let on_fault = Arc::clone(&on_fault);
        let transport = Arc::clone(&transport);
        Box::pin(async move {
            if closed.load(Ordering::SeqCst) {
                return;
            }
            let Some(message) = decode_wake_link_message(&frame.payload) else {
                on_fault(format!("an undecodable frame on {lane} was dropped"));
                return;
            };
            let commit_wake = |wake: WakeEvent| -> BoxFuture<'static, Result<(), String>> {
                let inbox = Arc::clone(&inbox);
                let hand = hand.clone();
                let check = check.clone();
                let on_fault = Arc::clone(&on_fault);
                let on_committed = Arc::clone(&on_committed);
                let transport = Arc::clone(&transport);
                let lane = lane.clone();
                let closed = Arc::clone(&closed);
                Box::pin(async move {
                    let check_arg = if wake.source == WAKE_DROPPED_SOURCE {
                        None
                    } else {
                        check
                    };
                    let receipt = inbox.receive(&hand, wake.clone(), check_arg).await?;
                    if let WakeReceipt::Refused {
                        reason: WakeRefusal::NotScopeSession,
                        ..
                    } = &receipt
                    {
                        let err = WakeRefusedError {
                            wake: wake.clone(),
                            reason: WakeRefusal::NotScopeSession,
                        };
                        on_fault(err.to_string());
                    }
                    if !closed.load(Ordering::SeqCst) {
                        let payload = encode_wake_link_message(&WakeLinkMessage::WakeReceipt {
                            receipt: receipt.clone(),
                        });
                        if let Err(err) = transport.send(&lane, payload).await {
                            on_fault(err.to_string());
                        }
                    }
                    if let WakeReceipt::Ack {
                        duplicate: false, ..
                    } = receipt
                    {
                        on_committed(wake);
                    }
                    Ok(())
                })
            };

            let res = match message {
                WakeLinkMessage::Wake { wake } => commit_wake(wake).await,
                WakeLinkMessage::WakeDropped {
                    event_id: Some(event_id),
                    scope,
                    reason,
                    count,
                } => {
                    commit_wake(WakeEvent {
                        event_id,
                        scope,
                        source: WAKE_DROPPED_SOURCE.to_owned(),
                        payload: serde_json::json!({
                            "reason": reason.as_str(),
                            "count": count,
                        }),
                    })
                    .await
                }
                WakeLinkMessage::WakeDropped {
                    event_id: None,
                    scope,
                    reason,
                    count,
                } => {
                    on_dropped(WakeDrop {
                        scope,
                        reason,
                        count,
                    });
                    Ok(())
                }
                _ => Ok(()),
            };
            if let Err(err) = res {
                on_fault(err);
            }
        })
    });

    link.transport.on_frame(handler);
    link
}
