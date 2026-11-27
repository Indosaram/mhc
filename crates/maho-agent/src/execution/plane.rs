//! Port of senpi `packages/agent/src/execution/plane.ts`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use maho_ai::types::BoxFuture;
use maho_ai::utils::abort::{AbortController, AbortSignal};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::transport::transport::Transport;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum PlaneMessage {
    #[serde(rename = "plane.request", rename_all = "camelCase")]
    Request {
        parent: String,
        child_request_id: String,
        plane: String,
        body: Value,
    },
    #[serde(rename = "plane.abort", rename_all = "camelCase")]
    Abort {
        parent: String,
        child_request_id: String,
    },
    #[serde(rename = "plane.reply", rename_all = "camelCase")]
    Reply {
        parent: String,
        child_request_id: String,
        body: Value,
    },
    #[serde(rename = "plane.refused", rename_all = "camelCase")]
    Refused {
        parent: String,
        child_request_id: String,
        reason: String,
    },
}

pub type PlaneHandler =
    Arc<dyn Fn(Value, AbortSignal) -> BoxFuture<'static, Result<Value, String>> + Send + Sync>;
pub type NestedRequestFn = Arc<
    dyn Fn(&str, Value, Option<AbortSignal>) -> BoxFuture<'static, Result<Value, PlaneRefusedError>>
        + Send
        + Sync,
>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("The brain refused this request: {reason}")]
pub struct PlaneRefusedError {
    pub reason: String,
}

impl PlaneRefusedError {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

pub fn encode_plane_message(message: &PlaneMessage) -> Vec<u8> {
    serde_json::to_vec(message).unwrap_or_default()
}

fn is_id(val: Option<&Value>) -> Option<&str> {
    val.and_then(Value::as_str).filter(|s| !s.is_empty())
}

pub fn decode_plane_message(payload: &[u8]) -> Option<PlaneMessage> {
    let value: Value = serde_json::from_slice(payload).ok()?;
    let obj = value.as_object()?;
    let kind = obj.get("kind")?.as_str()?;
    let parent = is_id(obj.get("parent"))?.to_owned();
    let child_request_id = is_id(obj.get("childRequestId"))?.to_owned();
    match kind {
        "plane.request" => {
            let plane = is_id(obj.get("plane"))?.to_owned();
            let body = obj.get("body")?.clone();
            Some(PlaneMessage::Request {
                parent,
                child_request_id,
                plane,
                body,
            })
        }
        "plane.abort" => Some(PlaneMessage::Abort {
            parent,
            child_request_id,
        }),
        "plane.reply" => {
            let body = obj.get("body")?.clone();
            Some(PlaneMessage::Reply {
                parent,
                child_request_id,
                body,
            })
        }
        "plane.refused" => {
            let reason = obj.get("reason")?.as_str()?.to_owned();
            Some(PlaneMessage::Refused {
                parent,
                child_request_id,
                reason,
            })
        }
        _ => None,
    }
}

pub type LiveLaneFn = Arc<dyn Fn(&str, Option<&str>) -> Option<String> + Send + Sync>;

pub struct PlaneServerOptions {
    pub transport: Arc<dyn Transport>,
    pub planes: HashMap<String, PlaneHandler>,
    pub live_lane: LiveLaneFn,
}

struct ServedEntry {
    controller: AbortController,
    answer: Option<PlaneMessage>,
}

type ServedMap = Arc<Mutex<HashMap<String, HashMap<String, Arc<Mutex<ServedEntry>>>>>>;

pub struct PlaneServer {
    transport: Arc<dyn Transport>,
    planes: HashMap<String, PlaneHandler>,
    live_lane: LiveLaneFn,
    served: ServedMap,
}

impl PlaneServer {
    fn send(&self, lane: &str, message: &PlaneMessage) {
        let transport = Arc::clone(&self.transport);
        let lane = lane.to_owned();
        let bytes = encode_plane_message(message);
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let _ = transport.send(&lane, bytes).await;
            });
        }
    }

    pub fn receive(&self, lane: &str, message: PlaneMessage) {
        match message {
            PlaneMessage::Request {
                parent,
                child_request_id,
                plane,
                body,
            } => {
                let refuse = |reason: &str| {
                    self.send(
                        lane,
                        &PlaneMessage::Refused {
                            parent: parent.clone(),
                            child_request_id: child_request_id.clone(),
                            reason: reason.to_owned(),
                        },
                    );
                };
                let Some(parent_lane) = (self.live_lane)(&parent, Some(&plane)) else {
                    refuse("parent_terminal");
                    return;
                };
                if parent_lane != lane {
                    refuse("scope_lane_mismatch");
                    return;
                }
                let entry_arc = {
                    let mut served = self.served.lock().unwrap_or_else(|p| p.into_inner());
                    let children = served.entry(parent.clone()).or_default();
                    if let Some(known) = children.get(&child_request_id) {
                        let known_guard = known.lock().unwrap_or_else(|p| p.into_inner());
                        if let Some(answer) = &known_guard.answer {
                            self.send(lane, answer);
                        }
                        return;
                    }
                    let Some(handler) = self.planes.get(&plane).cloned() else {
                        refuse("plane_unavailable");
                        return;
                    };
                    let entry = Arc::new(Mutex::new(ServedEntry {
                        controller: AbortController::new(),
                        answer: None,
                    }));
                    children.insert(child_request_id.clone(), Arc::clone(&entry));
                    (handler, entry)
                };

                let (handler, entry) = entry_arc;
                let signal = entry
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .controller
                    .signal();
                let served_map = Arc::clone(&self.served);
                let transport = Arc::clone(&self.transport);
                let lane_owned = lane.to_owned();

                if let Ok(rt) = tokio::runtime::Handle::try_current() {
                    rt.spawn(async move {
                        let answer = match handler(body, signal).await {
                            Ok(reply_body) => PlaneMessage::Reply {
                                parent: parent.clone(),
                                child_request_id: child_request_id.clone(),
                                body: reply_body,
                            },
                            Err(err) => PlaneMessage::Refused {
                                parent: parent.clone(),
                                child_request_id: child_request_id.clone(),
                                reason: format!("plane_failed: {err}"),
                            },
                        };
                        let still_matches = {
                            let guard = served_map.lock().unwrap_or_else(|p| p.into_inner());
                            guard
                                .get(&parent)
                                .and_then(|c| c.get(&child_request_id))
                                .is_some_and(|e| Arc::ptr_eq(e, &entry))
                        };
                        if !still_matches {
                            return;
                        }
                        entry.lock().unwrap_or_else(|p| p.into_inner()).answer =
                            Some(answer.clone());
                        let _ = transport
                            .send(&lane_owned, encode_plane_message(&answer))
                            .await;
                    });
                }
            }
            PlaneMessage::Abort {
                parent,
                child_request_id,
            } => {
                if (self.live_lane)(&parent, None).as_deref() == Some(lane) {
                    let entry = self
                        .served
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .get(&parent)
                        .and_then(|c| c.get(&child_request_id))
                        .cloned();
                    if let Some(entry) = entry {
                        entry
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .controller
                            .abort(None);
                    }
                }
            }
            PlaneMessage::Reply { .. } | PlaneMessage::Refused { .. } => {}
        }
    }

    pub fn parent_ended(&self, execution_id: &str) {
        let children = self
            .served
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(execution_id);
        if let Some(children) = children {
            for entry in children.values() {
                entry
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .controller
                    .abort(None);
            }
        }
    }
}

pub fn create_plane_server(options: PlaneServerOptions) -> Arc<PlaneServer> {
    Arc::new(PlaneServer {
        transport: options.transport,
        planes: options.planes,
        live_lane: options.live_lane,
        served: Arc::new(Mutex::new(HashMap::new())),
    })
}

pub struct PlaneClientOptions {
    pub transport: Arc<dyn Transport>,
    pub mint_id: Option<Arc<dyn Fn() -> String + Send + Sync>>,
}

struct PlaneWaiter {
    tx: oneshot::Sender<Result<Value, PlaneRefusedError>>,
}

pub struct PlaneClient {
    transport: Arc<dyn Transport>,
    mint_id: Arc<dyn Fn() -> String + Send + Sync>,
    waiting: Arc<Mutex<HashMap<String, HashMap<String, PlaneWaiter>>>>,
    live: Arc<Mutex<HashSet<String>>>,
}

impl PlaneClient {
    pub fn request_for(
        self: &Arc<Self>,
        parent: &str,
        lane: &str,
        parent_signal: AbortSignal,
    ) -> NestedRequestFn {
        self.live
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(parent.to_owned());
        let client = Arc::clone(self);
        let parent_owned = parent.to_owned();
        let lane_owned = lane.to_owned();

        Arc::new(
            move |plane: &str, body: Value, signal: Option<AbortSignal>| {
                let client = Arc::clone(&client);
                let parent = parent_owned.clone();
                let lane = lane_owned.clone();
                let plane = plane.to_owned();
                let parent_signal = parent_signal.clone();
                Box::pin(async move {
                    let is_live = client
                        .live
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .contains(&parent);
                    if !is_live
                        || parent_signal.aborted()
                        || signal.as_ref().is_some_and(AbortSignal::aborted)
                    {
                        return Err(PlaneRefusedError::new("parent_terminal"));
                    }
                    let child_request_id = (client.mint_id)();
                    let (tx, mut rx) = oneshot::channel();
                    {
                        let mut waiting = client.waiting.lock().unwrap_or_else(|p| p.into_inner());
                        waiting
                            .entry(parent.clone())
                            .or_default()
                            .insert(child_request_id.clone(), PlaneWaiter { tx });
                    }

                    let req = PlaneMessage::Request {
                        parent: parent.clone(),
                        child_request_id: child_request_id.clone(),
                        plane,
                        body,
                    };
                    if let Err(err) = client
                        .transport
                        .send(&lane, encode_plane_message(&req))
                        .await
                    {
                        let removed = client
                            .waiting
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .get_mut(&parent)
                            .and_then(|c| c.remove(&child_request_id));
                        if removed.is_some() {
                            return Err(PlaneRefusedError::new(format!(
                                "brain_unreachable: {err}"
                            )));
                        }
                    }

                    tokio::select! {
                        res = &mut rx => {
                            match res {
                                Ok(inner) => inner,
                                Err(_) => Err(PlaneRefusedError::new("parent_terminal")),
                            }
                        }
                        _ = parent_signal.cancelled() => {
                            let removed = client
                                .waiting
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .get_mut(&parent)
                                .and_then(|c| c.remove(&child_request_id));
                            if removed.is_some() {
                                let _ = client
                                    .transport
                                    .send(
                                        &lane,
                                        encode_plane_message(&PlaneMessage::Abort {
                                            parent,
                                            child_request_id,
                                        }),
                                    )
                                    .await;
                            }
                            Err(PlaneRefusedError::new("Operation aborted"))
                        }
                        _ = async {
                            match &signal {
                                Some(s) => s.cancelled().await,
                                None => std::future::pending::<()>().await,
                            }
                        } => {
                            let removed = client
                                .waiting
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .get_mut(&parent)
                                .and_then(|c| c.remove(&child_request_id));
                            if removed.is_some() {
                                let _ = client
                                    .transport
                                    .send(
                                        &lane,
                                        encode_plane_message(&PlaneMessage::Abort {
                                            parent,
                                            child_request_id,
                                        }),
                                    )
                                    .await;
                            }
                            Err(PlaneRefusedError::new("Operation aborted"))
                        }
                    }
                })
            },
        )
    }

    pub fn receive(&self, message: PlaneMessage) {
        let waiter = match &message {
            PlaneMessage::Reply {
                parent,
                child_request_id,
                ..
            }
            | PlaneMessage::Refused {
                parent,
                child_request_id,
                ..
            } => self
                .waiting
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get_mut(parent)
                .and_then(|c| c.remove(child_request_id)),
            _ => None,
        };
        let Some(waiter) = waiter else {
            return;
        };
        match message {
            PlaneMessage::Reply { body, .. } => {
                let _ = waiter.tx.send(Ok(body));
            }
            PlaneMessage::Refused { reason, .. } => {
                let _ = waiter.tx.send(Err(PlaneRefusedError::new(reason)));
            }
            _ => {}
        }
    }

    pub fn parent_ended(&self, parent: &str) {
        self.live
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(parent);
        let children = self
            .waiting
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(parent);
        if let Some(children) = children {
            for (_, waiter) in children {
                let _ = waiter
                    .tx
                    .send(Err(PlaneRefusedError::new("parent_terminal")));
            }
        }
    }
}

pub fn create_plane_client(options: PlaneClientOptions) -> Arc<PlaneClient> {
    Arc::new(PlaneClient {
        transport: options.transport,
        mint_id: options
            .mint_id
            .unwrap_or_else(|| Arc::new(|| Uuid::new_v4().to_string())),
        waiting: Arc::new(Mutex::new(HashMap::new())),
        live: Arc::new(Mutex::new(HashSet::new())),
    })
}
