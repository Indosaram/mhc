use super::{
    errors::ClientError,
    transport::{ByteTransport, ByteTransportFactory, ByteTransportHandlers},
    types::{ConnectionOptions, ConnectionState, ConnectionStateChange},
};
use crate::protocol::{
    codec::{MessageDecoder, MessageKind, encode_client_message},
    messages::PROTOCOL_VERSION,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

struct Active {
    id: u64,
    decoder: MessageDecoder,
    transport: Option<Arc<dyn ByteTransport>>,
    handshake: Option<oneshot::Sender<Result<Value, ClientError>>>,
}
enum Lifecycle {
    Disconnected,
    Connecting(Active),
    Connected(Active),
}
struct Inner {
    lifecycle: Lifecycle,
    sequence: u64,
}

pub struct Connection {
    options: ConnectionOptions,
    factory: Arc<dyn ByteTransportFactory>,
    inner: Mutex<Inner>,
}

impl Connection {
    pub fn new(
        options: ConnectionOptions,
        factory: Arc<dyn ByteTransportFactory>,
    ) -> Result<Arc<Self>, ClientError> {
        if options.max_frame_length == 0 {
            return Err(ClientError::InvalidOptions(
                "Client maxFrameLength must be an integer between 1 and 4294967295".into(),
            ));
        }
        Ok(Arc::new(Self {
            options,
            factory,
            inner: Mutex::new(Inner {
                lifecycle: Lifecycle::Disconnected,
                sequence: 0,
            }),
        }))
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub fn state(&self) -> ConnectionState {
        match &self.lock().lifecycle {
            Lifecycle::Disconnected => ConnectionState::Disconnected,
            Lifecycle::Connecting(_) => ConnectionState::Connecting,
            Lifecycle::Connected(_) => ConnectionState::Connected,
        }
    }
    pub fn max_frame_length(&self) -> u32 {
        self.options.max_frame_length
    }
    fn current(&self, id: u64) -> bool {
        match &self.lock().lifecycle {
            Lifecycle::Disconnected => false,
            Lifecycle::Connecting(a) | Lifecycle::Connected(a) => a.id == id,
        }
    }
    pub async fn connect(self: &Arc<Self>) -> Result<Value, ClientError> {
        let (tx, rx) = oneshot::channel();
        let id = {
            let mut inner = self.lock();
            match inner.lifecycle {
                Lifecycle::Disconnected => {}
                Lifecycle::Connecting(_) => {
                    return Err(ClientError::Disconnected(
                        "Client is already connecting".into(),
                    ));
                }
                Lifecycle::Connected(_) => {
                    return Err(ClientError::Disconnected(
                        "Client is already connected".into(),
                    ));
                }
            }
            inner.sequence = inner.sequence.wrapping_add(1);
            let id = inner.sequence;
            inner.lifecycle = Lifecycle::Connecting(Active {
                id,
                decoder: MessageDecoder::new(MessageKind::Server, self.options.max_frame_length),
                transport: None,
                handshake: Some(tx),
            });
            id
        };
        (self.options.on_state_change)(ConnectionStateChange {
            state: ConnectionState::Connecting,
            error: None,
        });
        let weak = Arc::downgrade(self);
        let close_weak = weak.clone();
        let error_weak = weak.clone();
        let handlers = ByteTransportHandlers {
            on_data: Arc::new(move |bytes| {
                if let Some(connection) = weak.upgrade() {
                    connection.data(id, bytes);
                }
            }),
            on_close: Arc::new(move || {
                if let Some(connection) = close_weak.upgrade() {
                    connection.closed(id);
                }
            }),
            on_error: Arc::new(move |error| {
                if let Some(connection) = error_weak.upgrade()
                    && connection.current(id)
                {
                    connection.fail(error);
                }
            }),
        };
        match self.factory.connect(handlers).await {
            Err(error) => {
                if self.current(id) {
                    self.fail(error);
                }
            }
            Ok(transport) => {
                let installed = {
                    let mut inner = self.lock();
                    match &mut inner.lifecycle {
                        Lifecycle::Connecting(active) if active.id == id => {
                            active.transport = Some(transport.clone());
                            true
                        }
                        Lifecycle::Disconnected
                        | Lifecycle::Connecting(_)
                        | Lifecycle::Connected(_) => false,
                    }
                };
                if !installed {
                    transport.close();
                } else {
                    match encode_client_message(
                        &json!({"type":"hello","version":PROTOCOL_VERSION}),
                        self.options.max_frame_length,
                    ) {
                        Ok(frame) => {
                            if let Err(error) = transport.send(&frame).await
                                && self.current(id)
                            {
                                self.fail(error);
                            }
                        }
                        Err(error) => self.fail(error.into()),
                    }
                }
            }
        }
        rx.await
            .map_err(|_| ClientError::Disconnected("Client disconnected".into()))?
    }
    pub async fn send(&self, frame: &[u8]) -> Result<(), ClientError> {
        let (transport, id) = {
            let inner = self.lock();
            match &inner.lifecycle {
                Lifecycle::Connected(a) => (
                    a.transport.clone().ok_or_else(|| {
                        ClientError::Disconnected("Client is disconnected".into())
                    })?,
                    a.id,
                ),
                Lifecycle::Disconnected | Lifecycle::Connecting(_) => {
                    return Err(ClientError::Disconnected("Client is disconnected".into()));
                }
            }
        };
        if let Err(error) = transport.send(frame).await {
            if self.current(id) {
                self.fail(error.clone());
            }
            return Err(error);
        }
        Ok(())
    }
    pub fn disconnect(&self, reason: &str) {
        self.fail(ClientError::Disconnected(reason.into()));
    }
    pub fn fail(&self, error: ClientError) {
        let active = {
            let mut inner = self.lock();
            match std::mem::replace(&mut inner.lifecycle, Lifecycle::Disconnected) {
                Lifecycle::Disconnected => return,
                Lifecycle::Connecting(a) | Lifecycle::Connected(a) => a,
            }
        };
        if let Some(handshake) = active.handshake {
            match handshake.send(Err(error.clone())) {
                Ok(()) | Err(_) => {}
            }
        }
        (self.options.on_state_change)(ConnectionStateChange {
            state: ConnectionState::Disconnected,
            error: Some(error),
        });
        if let Some(transport) = active.transport {
            transport.close();
        }
    }
    fn closed(&self, id: u64) {
        let error = {
            let mut inner = self.lock();
            match &mut inner.lifecycle {
                Lifecycle::Connecting(a) | Lifecycle::Connected(a) if a.id == id => a
                    .decoder
                    .end()
                    .err()
                    .map(ClientError::from)
                    .unwrap_or_else(|| ClientError::Disconnected("Byte transport closed".into())),
                Lifecycle::Disconnected | Lifecycle::Connecting(_) | Lifecycle::Connected(_) => {
                    return;
                }
            }
        };
        self.fail(error);
    }
    fn data(&self, id: u64, bytes: &[u8]) {
        let messages = {
            let mut inner = self.lock();
            match &mut inner.lifecycle {
                Lifecycle::Connecting(a) if a.id == id && a.transport.is_none() => {
                    Err(ClientError::Protocol(
                        "Received server data before the client hello was sent".into(),
                    ))
                }
                Lifecycle::Connecting(a) | Lifecycle::Connected(a) if a.id == id => {
                    a.decoder.push(bytes).map_err(ClientError::from)
                }
                Lifecycle::Disconnected | Lifecycle::Connecting(_) | Lifecycle::Connected(_) => {
                    return;
                }
            }
        };
        match messages {
            Err(error) => self.fail(error),
            Ok(messages) => {
                for message in messages {
                    if !self.current(id) {
                        return;
                    }
                    if let Err(error) = self.message(id, message) {
                        self.fail(error);
                        return;
                    }
                }
            }
        }
    }
    fn message(&self, id: u64, message: Value) -> Result<(), ClientError> {
        match self.state() {
            ConnectionState::Disconnected => Ok(()),
            ConnectionState::Connecting => {
                if message["type"] == "hello_error" {
                    return Err(ClientError::Server {
                        code: message["error"]["code"].as_str().unwrap_or_default().into(),
                        message: message["error"]["message"]
                            .as_str()
                            .unwrap_or_default()
                            .into(),
                    });
                }
                if message["type"] != "hello" {
                    return Err(ClientError::Protocol(
                        "Expected server hello as first message".into(),
                    ));
                }
                if message["serverId"].as_str() != Some(self.options.server_id.as_str()) {
                    return Err(ClientError::Protocol(format!(
                        "Connected server {} does not match {}",
                        message["serverId"],
                        json!(self.options.server_id)
                    )));
                }
                {
                    let mut inner = self.lock();
                    let previous = std::mem::replace(&mut inner.lifecycle, Lifecycle::Disconnected);
                    match previous {
                        Lifecycle::Connecting(a) if a.id == id => {
                            inner.lifecycle = Lifecycle::Connected(a)
                        }
                        other => {
                            inner.lifecycle = other;
                            return Ok(());
                        }
                    }
                }
                (self.options.on_handshake)(&message)?;
                if !self.current(id) {
                    return Ok(());
                }
                (self.options.on_state_change)(ConnectionStateChange {
                    state: ConnectionState::Connected,
                    error: None,
                });
                let handshake = {
                    let mut inner = self.lock();
                    match &mut inner.lifecycle {
                        Lifecycle::Connected(a) if a.id == id => a.handshake.take(),
                        Lifecycle::Disconnected
                        | Lifecycle::Connecting(_)
                        | Lifecycle::Connected(_) => None,
                    }
                };
                if let Some(handshake) = handshake {
                    match handshake.send(Ok(message)) {
                        Ok(()) | Err(_) => {}
                    }
                }
                Ok(())
            }
            ConnectionState::Connected => {
                if message["type"] == "hello" || message["type"] == "hello_error" {
                    return Err(ClientError::Protocol("Unexpected handshake message".into()));
                }
                (self.options.on_message)(message)
            }
        }
    }
}
