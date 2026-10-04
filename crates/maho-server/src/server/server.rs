use super::{
    errors::ServerError,
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
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex, mpsc, watch},
    task::JoinSet,
};

pub struct Server {
    pub server_id: String,
    host: Arc<dyn ServerHost>,
    router: Arc<SessionRouter>,
    max_frame_length: u32,
    handshake_timeout: Duration,
    closing: AtomicBool,
}
impl Server {
    pub fn new(
        host: Arc<dyn ServerHost>,
        server_id: String,
        max_frame_length: Option<u32>,
        handshake_timeout: Option<Duration>,
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
        let router = Arc::new(SessionRouter::new(host.clone(), server_id.clone()));
        router.bind_self();
        Ok(Arc::new(Self {
            router,
            server_id,
            host,
            max_frame_length,
            handshake_timeout,
            closing: AtomicBool::new(false),
        }))
    }
    pub async fn close(&self) -> Result<(), ServerError> {
        self.closing.store(true, Ordering::SeqCst);
        self.router.close().await
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
        let mut decoder = MessageDecoder::new(
            crate::protocol::codec::MessageKind::Client,
            self.max_frame_length,
        );
        let handshake = tokio::time::timeout(self.handshake_timeout, async {
            loop {
                let bytes = inbound.recv().await.ok_or_else(|| {
                    ServerError::new("invalid_request", "Byte transport closed")
                })??;
                let mut messages = decoder
                    .push(&bytes)
                    .map_err(|e| ServerError::new("invalid_request", &e.to_string()))?;
                if !messages.is_empty() {
                    let hello = messages.remove(0);
                    return Ok::<_, ServerError>((hello, messages));
                }
            }
        })
        .await
        .unwrap_or_else(|_| Err(ServerError::new("invalid_request", "Handshake timeout")));
        let (hello, remaining) = match handshake {
            Ok(hello) => hello,
            Err(error) => {
                connection.close(None).await?;
                return Err(error);
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
            let final_frame = encode_server_message(
                &json!({"type":"hello_error","error":{"code":error.code,"message":error.message}}),
                self.max_frame_length,
            )
            .map_err(|e| ServerError::new("invalid_request", &e.to_string()))?;
            return connection.close(Some(&final_frame)).await;
        }
        let presentation = Arc::new(Presentation {
            router: self.router.clone(),
            connection: connection.clone(),
            attachment: Mutex::new(None),
            max: self.max_frame_length,
        });
        let services = match self
            .host
            .server_services()
            .attach_client(presentation.clone())
            .await {
                Ok(services)=>services,
                Err(error)=>{
                    if let Err(release)=presentation.detach_session().await {eprintln!("{}",release.message);}
                    let final_frame=encode_server_message(
                        &json!({"type":"hello_error","error":{"code":error.code,"message":error.message}}),
                        self.max_frame_length,
                    );
                    if let Err(encode)=&final_frame {eprintln!("{encode}");}
                    if let Err(close)=connection.close(final_frame.as_ref().ok().map(Vec::as_slice)).await {eprintln!("{}",close.message);}
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
            send(
                &connection,
                &json!({"type":"hello","version":PROTOCOL_VERSION,"serverId":self.server_id}),
                self.max_frame_length,
            ).await?;
            loop {
                for message in pending.drain(..) {
                    match message["type"].as_str() {
                        Some("hello")=>return Err(ServerError::new("invalid_request","hello may only be sent as the first message")),
                        Some("cancel")=>{
                            if message["target"]["serverId"]==self.server_id {
                                let active=active.lock().await;
                                if let Some((target,cancel))=active.get(message["id"].as_str().unwrap_or_default()) && target==&message["target"] { cancel.send_replace(true); }
                            }
                        },
                        Some("request")=>{
                            let subscribing=(message["call"]["serviceId"]=="$chord.service" && message["call"]["member"]=="subscribe").then(||message["call"]["args"][0].as_str().unwrap_or_default().to_owned());
                            if let Some(subscription_id)=&subscribing && !subscriptions.lock().await.insert(subscription_id.clone()) {
                                send(&connection,&json!({"type":"response","id":message["id"],"ok":false,"error":{"code":"invalid_request","message":"Duplicate service subscription"}}),self.max_frame_length).await?;
                                continue;
                            }
                            let id=message["id"].as_str().unwrap_or_default().to_owned(); let (cancel,signal)=watch::channel(false);
                            { let mut active=active.lock().await;
                                if active.contains_key(&id) { if let Some(subscription_id)=&subscribing {subscriptions.lock().await.remove(subscription_id);} send(&connection,&json!({"type":"response","id":id,"ok":false,"error":{"code":"invalid_request","message":"Request ID is already active"}}),self.max_frame_length).await?; continue; }
                                active.insert(id.clone(),(message["target"].clone(),cancel));
                            }
                            let server=self.clone(); let connection=connection.clone(); let services=services.clone(); let presentation=presentation.clone(); let active=active.clone(); let encoders=encoders.clone();
                            let subscriptions=subscriptions.clone();
                            requests.spawn(async move {
                                let context=Context { cancelled:signal.clone() }; let mut cancelled=signal;
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
                                let result=tokio::select! { result=invoke=>result,_=cancelled.wait_for(|v| *v)=>Err(ServerError::new("cancelled","RPC request cancelled")) };
                                let result=match result {
                                    Ok(result) if subscribing.is_some()=>{
                                        let subscription_id=subscribing.as_ref().expect("subscription branch");
                                        let mut codecs=encoders.lock().await;
                                        if codecs.contains_key(subscription_id) { Err(ServerError::new("invalid_request","Duplicate service subscription")) }
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
                                    Err(error)=>json!({"type":"response","id":id,"ok":false,"error":{"code":error.code,"message":error.message}}),
                                };
                                if response["ok"]!=true && let Some(subscription_id)=&subscribing {subscriptions.lock().await.remove(subscription_id);}
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
                        _=>return Err(ServerError::new("invalid_request","Invalid client protocol message")),
                    }
                }
                tokio::select! {
                    _=shutdown.wait_for(|v| *v)=>break,
                    result=requests.join_next(),if !requests.is_empty()=>{ if let Some(result)=result { result.map_err(|e| ServerError::new("internal_error",&e.to_string()))??; } },
                    bytes=inbound.recv()=>match bytes {
                        Some(Ok(bytes))=>pending=decoder.push(&bytes).map_err(|e| ServerError::new("invalid_request",&e.to_string()))?,
                        Some(Err(error))=>return Err(error),
                        None=>{ decoder.end().map_err(|e| ServerError::new("invalid_request",&e.to_string()))?; break; },
                    }
                }
            }
            Ok(())
        }.await;
        for (_, (_, cancel)) in active.lock().await.iter() {
            cancel.send_replace(true);
        }
        while let Some(request) = requests.join_next().await {
            match request {
                Ok(Ok(())) => {}
                Ok(Err(error)) => eprintln!("{}", error.message),
                Err(error) => eprintln!("{error}"),
            }
        }
        let release = presentation.detach_session().await;
        let release_services = services.release().await;
        let close = connection.close(None).await;
        result.and(release).and(release_services).and(close)
    }
}

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
    router: Arc<SessionRouter>,
    connection: Arc<dyn ByteConnection>,
    attachment: Mutex<Option<Arc<Attachment>>>,
    max: u32,
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
            if let Some(previous) = current.take() {
                previous.release().await?;
            }
            send(
                &self.connection,
                &json!({"type":"attachment","attachment":attachment.target}),
                self.max,
            )
            .await?;
            *current = Some(attachment);
            Ok(())
        })
    }
    fn detach_session(&self) -> ServerFuture<'_, ()> {
        Box::pin(async move {
            let mut current = self.attachment.lock().await;
            if let Some(previous) = current.take() {
                previous.release().await?;
                send(
                    &self.connection,
                    &json!({"type":"attachment","attachment":null}),
                    self.max,
                )
                .await?;
            }
            Ok(())
        })
    }
    fn prepare_session_removal<'a>(&'a self, session_id: &'a str) -> ServerFuture<'a, ()> {
        Box::pin(async move {
            self.detach_session().await?;
            self.router.remove(session_id).await
        })
    }
}
