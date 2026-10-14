use super::{
    errors::{INTERNAL_SERVER_ERROR_MESSAGE, ServerError},
    listener::{ServerListener, ServerListeners},
    session_router::{Attachment, SessionRouter},
    types::*,
};
use crate::{
    client::service_wire::parse_service_call,
    protocol::{
        codec::{MessageDecoder, encode_server_message},
        framing::DEFAULT_MAX_FRAME_LENGTH,
        messages::{PROTOCOL_VERSION, is_server_id},
    },
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex, mpsc, watch},
    task::JoinSet,
};

/// Observer invoked with every internal server error (pinned `ServerOptions.onError`). The default
/// `Server::new` has no observer, and errors are then dropped like the pinned optional `onError`.
pub type ServerErrorObserver = Arc<dyn Fn(ServerError) + Send + Sync>;
/// Pinned `ServerOptions.onConnectionCountChanged`: called with the live connection count after
/// each connect and disconnect.
pub type ConnectionCountObserver = Arc<dyn Fn(usize) + Send + Sync>;

/// Pinned `reportError`: deliver to the observer, swallowing observer panics.
pub(crate) fn report_to(observer: Option<&ServerErrorObserver>, error: ServerError) {
    if let Some(observer) = observer {
        let observer = Arc::clone(observer);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || observer(error)));
    }
}

pub struct Server {
    pub server_id: String,
    host: Arc<dyn ServerHost>,
    router: Arc<SessionRouter>,
    max_frame_length: u32,
    handshake_timeout: Duration,
    closing: AtomicBool,
    listeners: Vec<Arc<dyn ServerListener>>,
    on_error: Option<ServerErrorObserver>,
    on_connection_count_changed: Option<ConnectionCountObserver>,
    connections: AtomicUsize,
    started: AtomicBool,
    listener_shutdown: Mutex<Option<watch::Sender<bool>>>,
    closed: watch::Sender<Option<Result<(), ServerError>>>,
    close_lock: Mutex<()>,
}
impl Server {
    pub fn new(
        host: Arc<dyn ServerHost>,
        server_id: String,
        max_frame_length: Option<u32>,
        handshake_timeout: Option<Duration>,
    ) -> Result<Arc<Self>, ServerError> {
        Self::with_options(host, server_id, max_frame_length, handshake_timeout, Vec::new(), None, None)
    }
    /// Pinned `ServerOptions` construction: the server owns its listener set and error observer
    /// (`listeners`/`onError`), exactly like the pinned `new Server(host, options)`.
    pub fn with_options(
        host: Arc<dyn ServerHost>,
        server_id: String,
        max_frame_length: Option<u32>,
        handshake_timeout: Option<Duration>,
        listeners: Vec<Arc<dyn ServerListener>>,
        on_error: Option<ServerErrorObserver>,
        on_connection_count_changed: Option<ConnectionCountObserver>,
    ) -> Result<Arc<Self>, ServerError> {
        if !is_server_id(&server_id) {
            return Err(ServerError::new(
                "invalid_request",
                "serverId must be a canonical lowercase UUIDv4",
            ));
        }
        let max_frame_length = max_frame_length.unwrap_or(DEFAULT_MAX_FRAME_LENGTH);
        if max_frame_length == 0 {
            return Err(ServerError::new(
                "invalid_request",
                "Server maxFrameLength must be an integer between 1 and 4294967295",
            ));
        }
        let handshake_timeout = handshake_timeout.unwrap_or(Duration::from_secs(5));
        if handshake_timeout.is_zero() || handshake_timeout.as_millis() > 2_147_483_647 {
            return Err(ServerError::new(
                "invalid_request",
                "Server handshakeTimeoutMs must be an integer between 1 and 2147483647",
            ));
        }
        let router_observer = on_error.clone();
        let router = Arc::new(SessionRouter::new(
            host.clone(),
            server_id.clone(),
            Arc::new(move |error| report_to(router_observer.as_ref(), error)),
        ));
        router.bind_self();
        let (closed, _) = watch::channel(None);
        Ok(Arc::new(Self {
            router,
            server_id,
            host,
            max_frame_length,
            handshake_timeout,
            closing: AtomicBool::new(false),
            listeners,
            on_error,
            on_connection_count_changed,
            connections: AtomicUsize::new(0),
            started: AtomicBool::new(false),
            listener_shutdown: Mutex::new(None),
            closed,
            close_lock: Mutex::new(()),
        }))
    }
    /// Report one internal server error to the pinned `onError` observer. Pinned `reportError`
    /// swallows observer exceptions: error observers cannot affect server state.
    pub(crate) fn report_error(&self, error: &ServerError) {
        report_to(self.on_error.as_ref(), error.clone());
    }
    /// Pinned `notifyConnectionCountChanged`: observer panics are reported, never propagated.
    fn notify_connection_count_changed(&self, count: usize) {
        if let Some(observer) = &self.on_connection_count_changed {
            let observer = Arc::clone(observer);
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || observer(count))).is_err() {
                self.report_error(&ServerError::new("internal_error", "Connection count observer failed"));
            }
        }
    }
    /// Pinned `failProtocol`: close the connection with one final `hello_error` frame. Encode and
    /// close failures are reported, never surfaced, like the pinned `closeConnection`.
    async fn fail_protocol(&self, connection: &Arc<dyn ByteConnection>, error: ServerError) {
        let final_frame = encode_server_message(
            &json!({"type":"hello_error","error":{"code":error.code,"message":error.message}}),
            self.max_frame_length,
        );
        if let Err(encode) = &final_frame {
            self.report_error(&ServerError::new("internal_error", &encode.to_string()));
        }
        if let Err(close) = connection.close(final_frame.as_ref().ok().map(Vec::as_slice)).await {
            self.report_error(&close);
        }
    }
    /// Pinned `toProtocolError`: only client-safe server, remote-service and protocol-validation
    /// errors reach the wire; anything else is reported and replaced by a generic internal error.
    pub(crate) fn to_protocol_error(&self, error: ServerError) -> ServerError {
        if PROTOCOL_ERROR_CODES.contains(&error.code.as_str()) {
            return error;
        }
        self.report_error(&error);
        ServerError::new("internal_error", INTERNAL_SERVER_ERROR_MESSAGE)
    }
    /// Pinned `Server.start()`: start the owned listeners, rolling back every already-started
    /// listener and the server state when any listener fails to start.
    pub async fn start(self: &Arc<Self>) -> Result<(), ServerError> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(ServerError::new("internal_error", "Server is already closed"));
        }
        if self.started.swap(true, Ordering::SeqCst) {
            return Err(ServerError::new("internal_error", "Server is already started"));
        }
        let (shutdown, signal) = watch::channel(false);
        *self.listener_shutdown.lock().await = Some(shutdown);
        match ServerListeners::new(self.listeners.clone()).start(self.clone(), signal).await {
            Ok(()) => Ok(()),
            Err(failure) => {
                // Pinned `startInternal` catch: mark closing, then close the server state; the
                // started listeners were already rolled back by `ServerListeners::start`.
                self.closing.store(true, Ordering::SeqCst);
                *self.listener_shutdown.lock().await = None;
                let mut cleanup_errors = failure.cleanup_errors;
                if let Err(error) = self.router.close().await {
                    cleanup_errors.push(error);
                }
                let start_error = failure.error;
                if cleanup_errors.is_empty() {
                    self.closed.send_replace(Some(Ok(())));
                    Err(start_error)
                } else {
                    let message = std::iter::once(start_error.message.as_str())
                        .chain(cleanup_errors.iter().map(|error| error.message.as_str()))
                        .collect::<Vec<_>>()
                        .join("; ");
                    let failure = ServerError::new("internal_error", &message);
                    self.closed.send_replace(Some(Err(failure.clone())));
                    Err(failure)
                }
            }
        }
    }
    pub async fn close(&self) -> Result<(), ServerError> {
        let _close = self.close_lock.lock().await;
        if let Some(result) = self.closed.borrow().clone() {
            return result;
        }
        self.closing.store(true, Ordering::SeqCst);
        if let Some(shutdown) = self.listener_shutdown.lock().await.take() {
            shutdown.send_replace(true);
        }
        let router = self.router.close().await;
        // Close the owned listeners, attempting every one and aggregating failures (pinned
        // `closeInternal`); an empty listener set is a no-op for the `Server::new` callers.
        let listeners = ServerListeners::new(self.listeners.clone()).close().await;
        let result = match (router, listeners) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(router_error), Err(listener_error)) => Err(ServerError::new(
                "internal_error",
                &format!("{}; {}", router_error.message, listener_error.message),
            )),
        };
        self.closed.send_replace(Some(result.clone()));
        result
    }
    pub async fn closed(&self) -> Result<(), ServerError> {
        let mut closed = self.closed.subscribe();
        loop {
            if let Some(result) = closed.borrow_and_update().clone() {
                return result;
            }
            if closed.changed().await.is_err() {
                return Err(ServerError::new("internal_error", "Server closed signal was dropped"));
            }
        }
    }
    pub fn max_frame_length(&self) -> u32 {
        self.max_frame_length
    }
    pub async fn serve(
        self: Arc<Self>,
        connection: Arc<dyn ByteConnection>,
        mut inbound: mpsc::Receiver<Result<Vec<u8>, ServerError>>,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), ServerError> {
        if self.closing.load(Ordering::SeqCst) {
            return connection.close(None).await;
        }
        let count = self.connections.fetch_add(1, Ordering::SeqCst) + 1;
        self.notify_connection_count_changed(count);
        // Every exit from `serve` is a disconnect (pinned `disconnect` deletes the connection
        // and notifies once).
        struct Connected<'a>(&'a Server);
        impl Drop for Connected<'_> {
            fn drop(&mut self) {
                let count = self.0.connections.fetch_sub(1, Ordering::SeqCst) - 1;
                self.0.notify_connection_count_changed(count);
            }
        }
        let _connected = Connected(self.as_ref());
        let mut decoder = MessageDecoder::new(
            crate::protocol::codec::MessageKind::Client,
            self.max_frame_length,
        );
        // A transport failure closes without a frame; a protocol failure (malformed frame or
        // handshake timeout) goes through pinned `failProtocol`.
        // A clean transport close before hello is quiet (pinned `transportClosed`).
        enum HandshakeFailure {
            Transport(ServerError),
            Protocol(ServerError),
            Closed,
        }
        // Pinned `accept` arms the handshake timer once and clears it only after the server
        // hello is sent, so one deadline covers hello, `attachClient` and the hello send.
        let handshake_deadline = tokio::time::Instant::now() + self.handshake_timeout;
        let handshake = tokio::time::timeout_at(handshake_deadline, async {
            loop {
                let bytes = inbound
                    .recv()
                    .await
                    .ok_or(HandshakeFailure::Closed)?
                    .map_err(HandshakeFailure::Transport)?;
                let mut messages = decoder.push(&bytes).map_err(|e| {
                    HandshakeFailure::Protocol(ServerError::new("invalid_request", &e.to_string()))
                })?;
                if !messages.is_empty() {
                    let hello = messages.remove(0);
                    return Ok::<_, HandshakeFailure>((hello, messages));
                }
            }
        })
        .await
        .unwrap_or_else(|_| {
            Err(HandshakeFailure::Protocol(ServerError::new(
                "invalid_request",
                "Handshake timeout",
            )))
        });
        let (hello, remaining) = match handshake {
            Ok(hello) => hello,
            Err(HandshakeFailure::Protocol(error)) => {
                self.fail_protocol(&connection, error).await;
                return Ok(());
            }
            Err(HandshakeFailure::Transport(error)) => {
                connection.close(None).await?;
                return Err(error);
            }
            Err(HandshakeFailure::Closed) => {
                if let Err(error) = decoder.end() {
                    self.report_error(&ServerError::new("invalid_request", &error.to_string()));
                }
                if let Err(error) = connection.close(None).await {
                    self.report_error(&error);
                }
                return Ok(());
            }
        };
        let hello_error = if hello["type"] != "hello" {
            Some(ServerError::new(
                "invalid_request",
                "The first client message must be hello",
            ))
        } else if hello["version"].as_u64() != Some(PROTOCOL_VERSION) {
            Some(ServerError::new(
                "version",
                &format!(
                    "Unsupported protocol version {}; expected {PROTOCOL_VERSION}",
                    hello["version"]
                ),
            ))
        } else {
            None
        };
        if let Some(error) = hello_error {
            self.fail_protocol(&connection, error).await;
            return Ok(());
        }
        let presentation = Arc::new_cyclic(|this| Presentation {
            this: this.clone(),
            router: self.router.clone(),
            connection: connection.clone(),
            attachment: Mutex::new(None),
            max: self.max_frame_length,
        });
        // The host attach runs as its own task so a handshake timeout can hand it off without
        // dropping it mid-acquisition (pinned `finishHandshake` keeps awaiting `attachClient`).
        let mut attaching = tokio::spawn({
            let host = self.host.clone();
            let presentation: Arc<dyn RoutedServerPresentation> = presentation.clone();
            async move { host.server_services().attach_client(presentation).await }
        });
        let services = match tokio::time::timeout_at(handshake_deadline, &mut attaching)
        .await {
                Err(_elapsed)=>{
                    // Pinned `finishHandshake`: services resolved after the handshake failed are
                    // released, never leaked.
                    self.fail_protocol(&connection,ServerError::new("invalid_request","Handshake timeout")).await;
                    let server=self.clone();
                    tokio::spawn(async move {
                        if let Ok(Ok(services))=attaching.await && let Err(error)=services.release().await {server.report_error(&error);}
                    });
                    if let Err(release)=presentation.release_silently().await {self.report_error(&release);}
                    return Ok(());
                },
                Ok(Err(join))=>{
                    let error=ServerError::new("internal_error",&join.to_string());
                    self.fail_protocol(&connection,self.to_protocol_error(error.clone())).await;
                    return Err(error);
                },
                Ok(Ok(Ok(services)))=>services,
                Ok(Ok(Err(error)))=>{
                    if let Err(release)=presentation.detach_session().await {self.report_error(&ServerError::new("internal_error",&release.message));}
                    self.fail_protocol(&connection,self.to_protocol_error(error.clone())).await;
                    return Err(error);
                },
            };
        let active = Arc::new(Mutex::new(
            BTreeMap::<String, (Value, watch::Sender<bool>)>::new(),
        ));
        let encoders = Arc::new(Mutex::new(BTreeMap::<
            String,
            super::state_codec::ServiceStateEncoder,
        >::new()));
        let mut requests = JoinSet::new();
        let subscriptions = Arc::new(Mutex::new(std::collections::BTreeSet::<String>::new()));
        let mut pending = remaining;
        let result=async {
            match tokio::time::timeout_at(handshake_deadline,send(
                &connection,
                &json!({"type":"hello","version":PROTOCOL_VERSION,"serverId":self.server_id}),
                self.max_frame_length,
            )).await {
                Err(_elapsed)=>return Ok(Some(ServerError::new("invalid_request","Handshake timeout"))),
                Ok(Err(error))=>{ self.report_error(&error); return Err(error); },
                Ok(Ok(()))=>{},
            }
            loop {
                for message in pending.drain(..) {
                    match message["type"].as_str() {
                        Some("hello")=>return Ok(Some(ServerError::new("invalid_request","hello may only be sent as the first message"))),
                        Some("cancel")=>{
                            if message["target"]["serverId"]==self.server_id {
                                let active=active.lock().await;
                                if let Some((target,cancel))=active.get(message["id"].as_str().unwrap_or_default()) && target==&message["target"] { cancel.send_replace(true); }
                            }
                        },
                        Some("request")=>{
                            let id=message["id"].as_str().unwrap_or_default().to_owned(); let (cancel,signal)=watch::channel(false);
                            { let mut active=active.lock().await;
                                if active.contains_key(&id) { send(&connection,&json!({"type":"response","id":id,"ok":false,"error":{"code":"invalid_request","message":"Request ID is already active"}}),self.max_frame_length).await?; continue; }
                                active.insert(id.clone(),(message["target"].clone(),cancel));
                            }
                            let subscribing=(message["call"]["serviceId"]=="$chord.service" && message["call"]["member"]=="subscribe").then(||message["call"]["args"][0].as_str().unwrap_or_default().to_owned());
                            // Pinned handleRequest order: parse the call and check the target server
                            // before rejecting a duplicate subscription id.
                            let reserved=match &subscribing {
                                Some(subscription_id) if parse_service_call(&message["call"]).is_ok() && message["target"]["serverId"]==self.server_id => {
                                    if !subscriptions.lock().await.insert(subscription_id.clone()) {
                                        active.lock().await.remove(&id);
                                        send(&connection,&json!({"type":"response","id":id,"ok":false,"error":{"code":"invalid_request","message":format!("Duplicate service subscription {subscription_id}")}}),self.max_frame_length).await?;
                                        continue;
                                    }
                                    Some(subscription_id.clone())
                                },
                                _=>None,
                            };
                            let server=self.clone(); let connection=connection.clone(); let services=services.clone(); let presentation=presentation.clone(); let active=active.clone(); let encoders=encoders.clone();
                            let subscriptions=subscriptions.clone();
                            requests.spawn(async move {
                                let context=Context { cancelled:signal.clone() }; let cancelled=signal;
                                let publisher_connection=connection.clone(); let max=server.max_frame_length;
                                let queued=Arc::new(Mutex::new(Some(Vec::<(String,Value)>::new())));
                                let pending=queued.clone();let publishing_encoders=encoders.clone();let subscribing_id=subscribing.clone();
                                let publish:Publisher=Arc::new(move |subscription_id,update| {
                                    let connection=publisher_connection.clone();let pending=pending.clone();let encoders=publishing_encoders.clone();let subscribing_id=subscribing_id.clone();
                                    Box::pin(async move {
                                        let mut pending=pending.lock().await;
                                        if subscribing_id.as_ref()==Some(&subscription_id) && let Some(updates)=pending.as_mut() { updates.push((subscription_id,update));return Ok(()); }
                                        let mut encoders=encoders.lock().await;
                                        let encoder=encoders.get_mut(&subscription_id).ok_or_else(||ServerError::new("invalid_request","Unknown service subscription"))?;
                                        let update=encoder.update(&update)?;
                                        send(&connection,&json!({"type":"service_update","subscriptionId":subscription_id,"update":update}),max).await
                                    })
                                });
                                let invoke=async {
                                    parse_service_call(&message["call"]).map_err(|_| ServerError::new("invalid_request","Invalid service call"))?;
                                    if message["target"]["serverId"]!=server.server_id { return Err(ServerError::wrong_server()); }
                                    if message["target"].get("sessionId").is_some() {
                                        let attachment=presentation.attachment.lock().await.clone().filter(|a| a.target==message["target"]).ok_or_else(ServerError::not_attached)?;
                                        attachment.invoke(message["call"].clone(),publish,context).await
                                    } else { services.invoke_service(message["call"].clone(),publish,context).await }
                                };
                                // Pinned `handleCancel` only aborts the context signal: the call
                                // runs to completion and the host decides how to honor it.
                                let result=invoke.await;
                                let result=match result {
                                    Ok(result) if subscribing.is_some()=>{
                                        let subscription_id=subscribing.as_ref().expect("subscription branch");
                                        let mut codecs=encoders.lock().await;
                                        if codecs.contains_key(subscription_id) { Err(ServerError::new("invalid_request",&format!("Duplicate service subscription {subscription_id}"))) }
                                        else if let Some(snapshot)=result {
                                            let mut encoder=super::state_codec::ServiceStateEncoder::default();
                                            match encoder.snapshot(&snapshot) { Ok(snapshot)=>{ codecs.insert(subscription_id.clone(),encoder);Ok(Some(snapshot)) },Err(error)=>Err(error) }
                                        } else { Err(ServerError::new("invalid_request","Service subscription did not return a snapshot")) }
                                    },
                                    result=>result,
                                };
                                let response=match result {
                                    Ok(Some(result))=>json!({"type":"response","id":id,"ok":true,"result":result}),
                                    Ok(None)=>json!({"type":"response","id":id,"ok":true}),
                                    Err(error)=>{
                                        let error=if *cancelled.borrow() {ServerError::new("cancelled","RPC request cancelled")} else {server.to_protocol_error(error)};
                                        json!({"type":"response","id":id,"ok":false,"error":{"code":error.code,"message":error.message}})
                                    },
                                };
                                if response["ok"]!=true && let Some(subscription_id)=&reserved {subscriptions.lock().await.remove(subscription_id);}
                                let sent:Result<(),ServerError>=async {
                                    send(&connection,&response,max).await?;
                                    if response["ok"]==true {
                                        if message["call"]["serviceId"]=="$chord.service" && message["call"]["member"]=="unsubscribe" { let subscription_id=message["call"]["args"][0].as_str().unwrap_or_default(); encoders.lock().await.remove(subscription_id); subscriptions.lock().await.remove(subscription_id); }
                                        let mut pending=queued.lock().await;
                                        if let Some(updates)=pending.take() { for (subscription_id,update) in updates {
                                            let mut codecs=encoders.lock().await;let codec=codecs.get_mut(&subscription_id).ok_or_else(||ServerError::new("invalid_request","Unknown service subscription"))?;
                                            let update=codec.update(&update)?;send(&connection,&json!({"type":"service_update","subscriptionId":subscription_id,"update":update}),max).await?;
                                        } }
                                    }
                                    Ok(())
                                }.await; active.lock().await.remove(&id); sent
                            });
                        },
                        _=>return Ok(Some(ServerError::new("invalid_request","Invalid client protocol message"))),
                    }
                }
                tokio::select! {
                    _=shutdown.wait_for(|v| *v)=>break,
                    result=requests.join_next(),if !requests.is_empty()=>{ if let Some(result)=result { result.map_err(|e| ServerError::new("internal_error",&e.to_string()))??; } },
                    bytes=inbound.recv()=>match bytes {
                        Some(Ok(bytes))=>match decoder.push(&bytes) {
                            Ok(messages)=>pending=messages,
                            Err(error)=>return Ok(Some(ServerError::new("invalid_request",&error.to_string()))),
                        },
                        Some(Err(error))=>return Err(error),
                        None=>{ decoder.end().map_err(|e| ServerError::new("invalid_request",&e.to_string()))?; break; },
                    }
                }
            }
            Ok(None)
        }.await;
        // A protocol failure closes with a final `hello_error` frame before teardown (pinned
        // `failProtocol` then `disconnect`).
        let (result, closed) = match result {
            Ok(Some(error)) => {
                self.fail_protocol(&connection, error).await;
                (Ok(()), true)
            }
            Ok(None) => (Ok(()), false),
            Err(error) => (Err(error), false),
        };
        for (_, (_, cancel)) in active.lock().await.iter() {
            cancel.send_replace(true);
        }
        while let Some(request) = requests.join_next().await {
            match request {
                Ok(Ok(())) => {}
                Ok(Err(error)) => self.report_error(&error),
                Err(error) => self.report_error(&ServerError::new("internal_error", &error.to_string())),
            }
        }
        // Pinned `disconnect` releases with publish=false: never write to a departing peer.
        let release = presentation.release_silently().await;
        let release_services = services.release().await;
        let close = if closed { Ok(()) } else { connection.close(None).await };
        result.and(release).and(release_services).and(close)
    }
}

/// Codes whose messages may cross the protocol boundary (pinned `ServerError`,
/// `RemoteServiceError` and `ProtocolValidationError`).
const PROTOCOL_ERROR_CODES: &[&str] = &[
    "service_not_allowed",
    "service_not_found",
    "service_mode_mismatch",
    "service_member_not_found",
    "service_member_mismatch",
    "service_instance_not_found",
    "service_stale_instance",
    "service_invalid_value",
    "wrong_server",
    "session_not_found",
    "session_ambiguous",
    "session_not_attached",
    "server_draining",
    "invalid_request",
];

pub async fn send(
    connection: &Arc<dyn ByteConnection>,
    message: &Value,
    max: u32,
) -> Result<(), ServerError> {
    let frame = encode_server_message(message, max)
        .map_err(|e| ServerError::new("invalid_request", &e.to_string()))?;
    connection.send(&frame).await
}
struct Presentation {
    this: std::sync::Weak<Presentation>,
    router: Arc<SessionRouter>,
    connection: Arc<dyn ByteConnection>,
    attachment: Mutex<Option<Arc<Attachment>>>,
    max: u32,
}
impl Presentation {
    /// Release the current attachment without publishing `attachment:null` (connection teardown).
    async fn release_silently(&self) -> Result<(), ServerError> {
        let previous = self.attachment.lock().await.take();
        match previous {
            Some(previous) => previous.release().await,
            None => Ok(()),
        }
    }
    /// Pinned `clearAttachment` with publish=true for router-initiated releases: forget the
    /// attachment if it is still current, then tell the client unless it already disconnected.
    async fn publish_detached(&self, detached: &std::sync::Weak<Attachment>) -> Result<(), ServerError> {
        let mut current = self.attachment.lock().await;
        if current.as_ref().is_none_or(|attachment| Arc::as_ptr(attachment) != detached.as_ptr()) {
            return Ok(());
        }
        current.take();
        if self.connection.closed() {
            return Ok(());
        }
        send(&self.connection, &json!({"type":"attachment","attachment":null}), self.max).await
    }
}
impl RoutedServerPresentation for Presentation {
    fn attach_session<'a>(&'a self, session_id: &'a str) -> ServerFuture<'a, ()> {
        Box::pin(async move {
            let mut current = self.attachment.lock().await;
            if current
                .as_ref()
                .is_some_and(|a| a.target["sessionId"] == session_id)
            {
                return Ok(());
            }
            let attachment = self.router.attach(session_id).await?;
            let presentation = self.this.clone();
            let detached = Arc::downgrade(&attachment);
            attachment.set_detach_notifier(Arc::new(move || {
                let presentation = presentation.clone();
                let detached = detached.clone();
                Box::pin(async move {
                    match presentation.upgrade() {
                        Some(presentation) => presentation.publish_detached(&detached).await,
                        None => Ok(()),
                    }
                }) as ServerFuture<'static, ()>
            }));
            // Pinned `attachClientNow` tracks the new attachment before publishing it, so a
            // failed release or publish leaves the lease owned and released at disconnect.
            let target = attachment.target.clone();
            if let Some(previous) = current.replace(attachment) {
                previous.release().await?;
            }
            send(
                &self.connection,
                &json!({"type":"attachment","attachment":target}),
                self.max,
            )
            .await
        })
    }
    fn detach_session(&self) -> ServerFuture<'_, ()> {
        Box::pin(async move {
            let mut current = self.attachment.lock().await;
            // Pinned `releaseAttachment`: `attachment:null` is published in `finally`, even
            // when the lease release fails, and the release error is then surfaced.
            let Some(previous) = current.take() else {
                return Ok(());
            };
            let released = previous.release().await;
            let published = if self.connection.closed() {
                Ok(())
            } else {
                send(
                    &self.connection,
                    &json!({"type":"attachment","attachment":null}),
                    self.max,
                )
                .await
            };
            released.and(published)
        })
    }
    fn prepare_session_removal<'a>(&'a self, session_id: &'a str) -> ServerFuture<'a, ()> {
        Box::pin(async move {
            // Pinned `removeSession` releases only that session's attachments; the router
            // publishes `attachment:null` to each owning client it detaches.
            self.router.remove(session_id).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observers_receive_reports_and_a_panicking_observer_cannot_escape() {
        let errors: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = Arc::clone(&errors);
        let host: Arc<dyn ServerHost> = Arc::new(crate::server::testing::TestServerHost::default());
        let server = Server::with_options(Arc::clone(&host), "00000000-0000-4000-8000-000000000001".into(), None, None, Vec::new(),
            Some(Arc::new(move |error: ServerError| captured.lock().expect("errors").push(error.message))), None).expect("server");
        server.report_error(&ServerError::new("internal_error", "boom"));
        assert_eq!(errors.lock().expect("errors").as_slice(), ["boom".to_string()]);
        let panicking = Server::with_options(host, "00000000-0000-4000-8000-000000000002".into(), None, None, Vec::new(),
            Some(Arc::new(|_: ServerError| panic!("observer failure"))), None).expect("server");
        panicking.report_error(&ServerError::new("internal_error", "ignored"));
    }
}
