use super::{
    connection::Connection,
    errors::ClientError,
    service_wire::{ServiceStateDecoder, parse_catalogue, parse_service_call},
    transport::ByteTransportFactory,
    types::{ConnectionOptions, ConnectionState, ConnectionStateChange},
};
use crate::protocol::{codec::encode_client_message, messages::is_server_id};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, oneshot, watch};

type Reply = oneshot::Sender<Result<Option<Value>, ClientError>>;
struct Pending {
    reply: Option<Reply>,
    subscription: Option<String>,
}
struct SubscriptionState {
    decoder: ServiceStateDecoder,
    hydrated: bool,
    ready: bool,
    wire: Vec<Value>,
    queued: Vec<Value>,
    updates: mpsc::UnboundedSender<Value>,
}
struct State {
    disposed: bool,
    sequence: u64,
    subscription_sequence: u64,
    hello: Option<Value>,
    attachment: Option<Value>,
    pending: BTreeMap<String, Pending>,
    subscriptions: BTreeMap<String, SubscriptionState>,
}

pub struct Client {
    connection: Arc<Connection>,
    state: Arc<Mutex<State>>,
    connection_events: watch::Sender<ConnectionStateChange>,
    attachment_events: watch::Sender<Option<Value>>,
}

fn lock(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Client {
    pub fn new(
        server_id: String,
        max: u32,
        factory: Arc<dyn ByteTransportFactory>,
    ) -> Result<Arc<Self>, ClientError> {
        if !is_server_id(&server_id) {
            return Err(ClientError::InvalidOptions(
                "serverId must be a canonical lowercase UUIDv4".into(),
            ));
        }
        let state = Arc::new(Mutex::new(State {
            disposed: false,
            sequence: 0,
            subscription_sequence: 0,
            hello: None,
            attachment: None,
            pending: BTreeMap::new(),
            subscriptions: BTreeMap::new(),
        }));
        let (connection_events, _) = watch::channel(ConnectionStateChange {
            state: ConnectionState::Disconnected,
            error: None,
        });
        let (attachment_events, _) = watch::channel(None);
        let handshake_state = state.clone();
        let message_state = state.clone();
        let change_state = state.clone();
        let changes = connection_events.clone();
        let attachment_changes = attachment_events.clone();
        let message_attachments = attachment_events.clone();
        let expected = server_id.clone();
        let connection = Connection::new(
            ConnectionOptions {
                server_id,
                max_frame_length: max,
                on_handshake: Arc::new(move |hello| {
                    lock(&handshake_state).hello = Some(hello.clone());
                    Ok(())
                }),
                on_message: Arc::new(move |message| {
                    handle_message(&message_state, &expected, &message_attachments, message)
                }),
                on_state_change: Arc::new(move |change| {
                    if change.state == ConnectionState::Disconnected {
                        let mut state = lock(&change_state);
                        state.hello = None;
                        state.attachment = None;
                        let pending = std::mem::take(&mut state.pending);
                        state.subscriptions.clear();
                        for (_, request) in pending {
                            if let Some(reply) = request.reply {
                                match reply.send(Err(change.error.clone().unwrap_or_else(|| {
                                    ClientError::Disconnected("Client is disconnected".into())
                                }))) {
                                    Ok(()) | Err(_) => {}
                                }
                            }
                        }
                        drop(state);
                        attachment_changes.send_if_modified(|v| {
                            if v.is_some() {
                                *v = None;
                                true
                            } else {
                                false
                            }
                        });
                    }
                    changes.send_replace(change);
                }),
            },
            factory,
        )?;
        Ok(Arc::new(Self {
            connection,
            state,
            connection_events,
            attachment_events,
        }))
    }
    pub async fn connect(self: &Arc<Self>) -> Result<Value, ClientError> {
        {
            let mut state = lock(&self.state);
            if state.disposed {
                return Err(ClientError::Disposed);
            }
            state.hello = None;
        }
        self.connection.connect().await
    }
    pub fn connected(&self) -> bool {
        self.connection.state() == ConnectionState::Connected
    }
    pub fn connection_state(&self) -> ConnectionState {
        self.connection.state()
    }
    pub fn hello(&self) -> Option<Value> {
        lock(&self.state).hello.clone()
    }
    pub fn attachment(&self) -> Option<Value> {
        lock(&self.state).attachment.clone()
    }
    pub fn connection_changes(&self) -> watch::Receiver<ConnectionStateChange> {
        self.connection_events.subscribe()
    }
    pub fn attachment_changes(&self) -> watch::Receiver<Option<Value>> {
        self.attachment_events.subscribe()
    }
    pub fn disconnect(&self, reason: &str) {
        self.connection.disconnect(reason);
    }
    pub fn dispose(&self) {
        lock(&self.state).disposed = true;
        self.connection.fail(ClientError::Disposed);
    }
    pub async fn request(
        &self,
        target: Value,
        call: Value,
        mut cancellation: Option<watch::Receiver<Option<ClientError>>>,
    ) -> Result<Option<Value>, ClientError> {
        if let Some(cancel) = &cancellation
            && let Some(reason) = cancel.borrow().clone()
        {
            return Err(reason);
        }
        let (id, rx) = self.begin_request(target.clone(), call, None).await?;
        if let Some(cancel) = cancellation.as_mut() {
            tokio::select! {
                result=rx=>result.map_err(|_| ClientError::Disconnected("Client is disconnected".into()))?,
                reason=async { cancel.wait_for(|v| v.is_some()).await.map(|value| value.clone()) }=>{
                    let reason=reason.ok().flatten().unwrap_or_else(|| ClientError::Disconnected("The operation was aborted".into()));
                    if let Some(request)=lock(&self.state).pending.get_mut(&id) { request.reply=None; }
                    if self.connected() {
                        let frame=encode_client_message(&json!({"type":"cancel","id":id,"target":target}),self.connection.max_frame_length())?;
                        self.connection.send(&frame).await?;
                    }
                    Err(reason)
                }
            }
        } else {
            rx.await
                .map_err(|_| ClientError::Disconnected("Client is disconnected".into()))?
        }
    }
    async fn begin_request(
        &self,
        target: Value,
        call: Value,
        subscription: Option<String>,
    ) -> Result<
        (
            String,
            oneshot::Receiver<Result<Option<Value>, ClientError>>,
        ),
        ClientError,
    > {
        let (tx, rx) = oneshot::channel();
        let id = {
            let mut state = lock(&self.state);
            if state.disposed {
                return Err(ClientError::Disposed);
            }
            if !self.connected() {
                return Err(ClientError::Disconnected("Client is disconnected".into()));
            }
            state.sequence = state.sequence.wrapping_add(1);
            format!("request-{}", state.sequence)
        };
        parse_service_call(&call)?;
        let frame = encode_client_message(
            &json!({"type":"request","id":id,"target":target,"call":call}),
            self.connection.max_frame_length(),
        )?;
        lock(&self.state).pending.insert(
            id.clone(),
            Pending {
                reply: Some(tx),
                subscription,
            },
        );
        self.connection.send(&frame).await?;
        Ok((id, rx))
    }
    pub async fn service_catalogue(&self, target: Value) -> Result<Value, ClientError> {
        let result = self
            .request(
                target,
                json!({"serviceId":"$chord.service","member":"catalogue","args":[]}),
                None,
            )
            .await?
            .unwrap_or(Value::Null);
        if let Err(error) = parse_catalogue(&result) {
            self.connection.fail(error.clone());
            return Err(error);
        }
        Ok(result)
    }
    pub async fn subscribe(
        self: &Arc<Self>,
        target: Value,
        service_id: &str,
        mode: &str,
    ) -> Result<ServiceSubscription, ClientError> {
        let (updates, rx) = mpsc::unbounded_channel();
        let id = {
            let mut state = lock(&self.state);
            state.subscription_sequence = state.subscription_sequence.wrapping_add(1);
            let id = format!("service-{}", state.subscription_sequence);
            state.subscriptions.insert(
                id.clone(),
                SubscriptionState {
                    decoder: ServiceStateDecoder::default(),
                    hydrated: false,
                    ready: false,
                    wire: Vec::new(),
                    queued: Vec::new(),
                    updates,
                },
            );
            id
        };
        let result=async {
            let (_,reply)=self.begin_request(target.clone(),json!({"serviceId":"$chord.service","member":"subscribe","args":[id,service_id,mode]}),Some(id.clone())).await?;
            reply.await.map_err(|_| ClientError::Disconnected("Client is disconnected".into()))?
        }.await;
        match result {
            Ok(Some(snapshot)) => Ok(ServiceSubscription {
                id,
                target,
                snapshot,
                updates: rx,
                client: self.clone(),
                disposed: false,
            }),
            Ok(None) => {
                lock(&self.state).subscriptions.remove(&id);
                Err(ClientError::Protocol(
                    "Invalid service subscription snapshot".into(),
                ))
            }
            Err(error) => {
                lock(&self.state).subscriptions.remove(&id);
                Err(error)
            }
        }
    }
}

pub struct ServiceSubscription {
    pub id: String,
    pub target: Value,
    pub snapshot: Value,
    pub updates: mpsc::UnboundedReceiver<Value>,
    client: Arc<Client>,
    disposed: bool,
}
impl ServiceSubscription {
    pub fn start(&self) {
        let mut state = lock(&self.client.state);
        if let Some(subscription) = state.subscriptions.get_mut(&self.id) {
            if subscription.ready {
                return;
            }
            subscription.ready = true;
            for update in subscription.queued.drain(..) {
                if subscription.updates.send(update).is_err() {}
            }
        }
    }
    pub async fn dispose(&mut self) -> Result<(), ClientError> {
        if self.disposed {
            return Ok(());
        }
        self.disposed = true;
        lock(&self.client.state).subscriptions.remove(&self.id);
        let current = if self.target.get("sessionId").is_some() {
            self.client.attachment().as_ref() == Some(&self.target)
        } else {
            self.client
                .hello()
                .is_some_and(|v| v["serverId"] == self.target["serverId"])
        };
        if self.client.connected() && current {
            self.client
                .request(
                    self.target.clone(),
                    json!({"serviceId":"$chord.service","member":"unsubscribe","args":[self.id]}),
                    None,
                )
                .await?;
        }
        Ok(())
    }
}

fn handle_message(
    state: &Mutex<State>,
    server_id: &str,
    attachments: &watch::Sender<Option<Value>>,
    message: Value,
) -> Result<(), ClientError> {
    let mut state = lock(state);
    match message["type"].as_str() {
        Some("attachment") => {
            let value = &message["attachment"];
            if !value.is_null() && value["serverId"].as_str() != Some(server_id) {
                return Err(ClientError::Protocol(
                    "Attachment update belongs to another server".into(),
                ));
            }
            let value = if value.is_null() {
                None
            } else {
                Some(value.clone())
            };
            if state.attachment != value {
                state.attachment = value.clone();
                attachments.send_replace(value);
            }
        }
        Some("service_update") => {
            let id = message["subscriptionId"].as_str().unwrap_or_default();
            if let Some(active) = state.subscriptions.get_mut(id) {
                if !active.hydrated {
                    active.wire.push(message["update"].clone());
                } else {
                    let update = active.decoder.update(&message["update"])?;
                    if active.ready {
                        if active.updates.send(update).is_err() {}
                    } else {
                        active.queued.push(update);
                    }
                }
            }
        }
        Some("response") => {
            let id = message["id"].as_str().unwrap_or_default();
            let pending = state
                .pending
                .remove(id)
                .ok_or_else(|| ClientError::Protocol("Response has no matching request".into()))?;
            let transformed = pending.subscription.is_some();
            let result = (|| {
                if message["ok"] == false {
                    return Err(ClientError::Server {
                        code: message["error"]["code"].as_str().unwrap_or_default().into(),
                        message: message["error"]["message"]
                            .as_str()
                            .unwrap_or_default()
                            .into(),
                    });
                }
                if let Some(id) = pending.subscription {
                    let active = state.subscriptions.get_mut(&id).ok_or_else(|| {
                        ClientError::Disconnected("Client is disconnected".into())
                    })?;
                    let snapshot = active.decoder.snapshot(&message["result"])?;
                    active.hydrated = true;
                    for update in active.wire.drain(..) {
                        active.queued.push(active.decoder.update(&update)?);
                    }
                    Ok(Some(snapshot))
                } else {
                    Ok(message.get("result").cloned())
                }
            })();
            let failure = if transformed && matches!(&result, Err(ClientError::Protocol(_))) {
                result.as_ref().err().cloned()
            } else {
                None
            };
            if let Some(reply) = pending.reply {
                match reply.send(result) {
                    Ok(()) | Err(_) => {}
                }
            }
            if let Some(error) = failure {
                return Err(error);
            }
        }
        _ => return Err(ClientError::Protocol("Unexpected server message".into())),
    }
    Ok(())
}
