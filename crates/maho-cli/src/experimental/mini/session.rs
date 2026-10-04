//! Port of senpi `experimental/mini/tui/session.ts`.
//!
//! The presentation's half of a session: one connection, services by token, one replicated snapshot.
//! `Sessions` is answered by the server and `Lane`/`Models` by the worker, but both are reached
//! through this one peer, so the view never learns which host provides what. The fold is the
//! harness's `reduce_lane_snapshot`: a replica must not have a second opinion.

use std::sync::{Arc, Mutex, Weak};

use maho_agent::harness::runtime::reducer::{reduce_lane_snapshot, LaneSnapshotReduction};
use serde_json::{json, Value};

use super::shared::protocol::{
    AuthEventPayload, CommandResult, LaneEvent, LaneSubscription, ModelRef, ModelsEvent, SessionSnapshot,
    SessionSummary, LANE, MODELS,
};
use super::shared::rpc::{create_peer, CallOptions, PeerOptions, RpcPeer};
use super::shared::transport::SocketTransport;

#[cfg(unix)]
async fn open_peer(transport: &SocketTransport) -> Result<RpcPeer, String> {
    let socket = tokio::net::UnixStream::connect(&transport.path).await.map_err(|error| error.to_string())?;
    let (input, output) = socket.into_split();
    Ok(create_peer(input, output, PeerOptions::default()))
}

#[cfg(unix)]
pub async fn list_sessions(transport: &SocketTransport) -> Result<Vec<SessionSummary>, String> {
    let peer = open_peer(transport).await?;
    let result = peer.call("sessions.list", Vec::new()).await?;
    serde_json::from_value(result).map_err(|error| error.to_string())
}

pub async fn attach(peer: &RpcPeer, session_id: Option<&str>, cwd: &str, presentation_id: &str) -> Result<String, String> {
    let value = peer.call_with(CallOptions { timeout_ms: Some(60_000), signal: None }, "sessions.attach", vec![json!(session_id), json!(cwd), json!(presentation_id)]).await?;
    serde_json::from_value(value).map_err(|error| error.to_string())
}

type Listener = Arc<dyn Fn() + Send + Sync>;
type AuthHandler = Arc<dyn Fn(AuthEventPayload) + Send + Sync>;

struct Shared {
    peer: Arc<RpcPeer>,
    presentation_id: String,
    state: Mutex<Option<SessionSnapshot>>,
    subscription_id: Mutex<Option<String>>,
    listeners: Mutex<Vec<Listener>>,
    auth_handlers: Mutex<Vec<AuthHandler>>,
    resubscribe: tokio::sync::Mutex<()>,
}

impl Shared {
    fn publish(&self) {
        let listeners = self.listeners.lock().unwrap_or_else(|error| error.into_inner()).clone();
        for listener in listeners {
            listener();
        }
    }
}

pub struct Subscription {
    shared: Weak<Shared>,
    listener: Listener,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.upgrade() {
            shared
                .listeners
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .retain(|candidate| !Arc::ptr_eq(candidate, &self.listener));
        }
    }
}

pub struct AttachedSession {
    shared: Arc<Shared>,
}

fn handle_lane_event(shared: &Arc<Shared>, payload: &Value) {
    let Ok(event) = serde_json::from_value::<LaneEvent>(payload.clone()) else { return; };
    let current = shared.subscription_id.lock().unwrap_or_else(|error| error.into_inner()).clone();
    if current.as_deref() != Some(event.subscription_id.as_str()) {
        return;
    }
    let rebase = {
        let mut state = shared.state.lock().unwrap_or_else(|error| error.into_inner());
        let Some(snapshot) = state.as_mut() else { return; };
        reduce_lane_snapshot(&mut snapshot.lane, &event.event) == Some(LaneSnapshotReduction::Rebase)
    };
    if rebase {
        let shared = shared.clone();
        tokio::spawn(async move {
            let _ = resubscribe(&shared).await;
        });
        return;
    }
    shared.publish();
}

fn handle_models_event(shared: &Arc<Shared>, payload: &Value) {
    let Ok(event) = serde_json::from_value::<ModelsEvent>(payload.clone()) else { return; };
    match &event {
        ModelsEvent::State { state } => {
            {
                let mut snapshot = shared.state.lock().unwrap_or_else(|error| error.into_inner());
                if let Some(current) = snapshot.as_mut() {
                    current.models = state.clone();
                }
            }
            shared.publish();
        }
        _ => {
            if let Some(payload) = AuthEventPayload::from_models_event(&event) {
                let handlers = shared.auth_handlers.lock().unwrap_or_else(|error| error.into_inner()).clone();
                for handler in handlers {
                    handler(payload.clone());
                }
            }
        }
    }
}

async fn resubscribe(shared: &Arc<Shared>) -> Result<(), String> {
    let _guard = shared.resubscribe.lock().await;
    let previous = shared.subscription_id.lock().unwrap_or_else(|error| error.into_inner()).clone();
    let value = shared.peer.call("lane.watch", vec![json!(shared.presentation_id)]).await?;
    let opened: LaneSubscription = serde_json::from_value(value).map_err(|error| error.to_string())?;
    {
        *shared.state.lock().unwrap_or_else(|error| error.into_inner()) = Some(opened.snapshot);
        *shared.subscription_id.lock().unwrap_or_else(|error| error.into_inner()) = Some(opened.subscription_id.clone());
    }
    shared.publish();
    shared.peer.call("lane.start", vec![json!(opened.subscription_id)]).await?;
    if let Some(previous) = previous {
        let _ = shared.peer.call("lane.unwatch", vec![json!(previous)]).await;
    }
    Ok(())
}

#[cfg(unix)]
pub async fn connect(transport: &SocketTransport, session_id: Option<&str>, cwd: &str) -> Result<AttachedSession, String> {
    let peer = Arc::new(open_peer(transport).await?);
    let shared = Arc::new(Shared {
        peer: peer.clone(),
        presentation_id: maho_ai::utils::uuid::uuidv7(None).unwrap_or_default(),
        state: Mutex::new(None),
        subscription_id: Mutex::new(None),
        listeners: Mutex::new(Vec::new()),
        auth_handlers: Mutex::new(Vec::new()),
        resubscribe: tokio::sync::Mutex::new(()),
    });
    {
        let weak = Arc::downgrade(&shared);
        peer.on(LANE, move |payload| {
            if let Some(shared) = weak.upgrade() {
                handle_lane_event(&shared, payload);
            }
        });
    }
    {
        let weak = Arc::downgrade(&shared);
        peer.on(MODELS, move |payload| {
            if let Some(shared) = weak.upgrade() {
                handle_models_event(&shared, payload);
            }
        });
    }
    attach(peer.as_ref(), session_id, cwd, &shared.presentation_id).await?;
    resubscribe(&shared).await?;
    Ok(AttachedSession { shared })
}

impl AttachedSession {
    pub fn state(&self) -> SessionSnapshot {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .expect("attached session has a snapshot")
    }

    pub fn subscribe(&self, listener: impl Fn() + Send + Sync + 'static) -> Subscription {
        let listener: Listener = Arc::new(listener);
        self.shared.listeners.lock().unwrap_or_else(|error| error.into_inner()).push(listener.clone());
        Subscription { shared: Arc::downgrade(&self.shared), listener }
    }

    pub fn on_auth(&self, handler: impl Fn(AuthEventPayload) + Send + Sync + 'static) {
        self.shared.auth_handlers.lock().unwrap_or_else(|error| error.into_inner()).push(Arc::new(handler));
    }

    async fn command(&self, method: &str, args: Vec<Value>) -> Result<CommandResult, String> {
        let value = self.shared.peer.call(method, args).await?;
        serde_json::from_value(value).map_err(|error| error.to_string())
    }

    pub async fn prompt(&self, text: &str) -> Result<CommandResult, String> {
        self.command("lane.prompt", vec![json!(text)]).await
    }

    pub async fn steer(&self, text: &str) -> Result<CommandResult, String> {
        self.command("lane.steer", vec![json!(text)]).await
    }

    pub async fn follow_up(&self, text: &str) -> Result<CommandResult, String> {
        self.command("lane.followUp", vec![json!(text)]).await
    }

    pub async fn compact(&self) -> Result<CommandResult, String> {
        self.command("lane.compact", Vec::new()).await
    }

    pub async fn abort(&self) -> Result<CommandResult, String> {
        self.command("lane.abort", Vec::new()).await
    }

    pub async fn set_model(&self, reference: &ModelRef) -> Result<CommandResult, String> {
        let value = serde_json::to_value(reference).map_err(|error| error.to_string())?;
        self.command("lane.setModel", vec![value]).await
    }

    pub async fn models_refresh(&self) -> Result<CommandResult, String> {
        self.command("models.refresh", Vec::new()).await
    }

    pub async fn models_auth_reply(&self, request_id: &str, answer: Option<&str>) -> Result<(), String> {
        self.shared.peer.call("models.authReply", vec![json!(request_id), json!(answer)]).await.map(|_| ())
    }

    pub fn close(&self) {
        self.shared.peer.close();
    }
}
