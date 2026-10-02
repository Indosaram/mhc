use std::{collections::{HashMap, HashSet}, future::Future, pin::Pin, sync::{Arc, Mutex}, time::Duration};
use maho_ai::utils::abort::{AbortController, AbortReason, AbortSignal};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{io::{AsyncRead, AsyncWrite, AsyncWriteExt}, sync::{mpsc, oneshot}};
use super::{protocol::ServiceToken, transport::JsonConnection};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Frame {
    Call { id: u64, method: String, args: Vec<Value> },
    Result { id: u64, result: Value },
    Error { id: u64, error: String },
    Cancel { id: u64 },
    Event { service: String, payload: Value, #[serde(skip_serializing_if = "Option::is_none")] to: Option<String> },
    Announce { services: Vec<String> },
    Ping,
}
pub type HandlerFuture = Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>;
pub type Handler = Arc<dyn Fn(Vec<Value>, AbortSignal) -> HandlerFuture + Send + Sync>;
pub type Forward = Arc<dyn Fn(String, Vec<Value>) -> HandlerFuture + Send + Sync>;
pub type EventHandler = Arc<dyn Fn(&str, &Value, Option<&str>) + Send + Sync>;
#[derive(Default)]
pub struct CallOptions { pub signal: Option<AbortSignal>, pub timeout_ms: Option<u64> }
pub struct PeerOptions { pub forward: Option<Forward>, pub dead_ms: u64 }
impl Default for PeerOptions { fn default() -> Self { Self { forward: None, dead_ms: 15_000 } } }
struct State {
    services: HashMap<String, HashMap<String, Handler>>, provided: Vec<String>, announced: HashSet<String>,
    pending: HashMap<u64, oneshot::Sender<Result<Value, String>>>, inflight: HashMap<u64, AbortController>,
    events: Vec<EventHandler>, close_handlers: Vec<Arc<dyn Fn() + Send + Sync>>, next_id: u64, closed: bool,
}
pub struct RpcPeer { state: Arc<Mutex<State>>, outgoing: mpsc::UnboundedSender<Frame>, close: AbortController }
impl RpcPeer {
    pub fn provide(&self, token: ServiceToken, implementation: HashMap<String, Handler>) {
        let services = {
            let mut state = self.state.lock().expect("RPC state lock");
            state.services.insert(token.name.to_owned(), implementation);
            if !state.provided.iter().any(|name| name == token.name) { state.provided.push(token.name.to_owned()); }
            state.provided.clone()
        };
        self.send(Frame::Announce { services });
    }
    pub fn provided(&self) -> Vec<String> { self.state.lock().expect("RPC state lock").provided.clone() }
    pub fn announced(&self) -> HashSet<String> { self.state.lock().expect("RPC state lock").announced.clone() }
    pub fn emit(&self, token: ServiceToken, payload: Value) { self.emit_raw(token.name, payload, None); }
    pub fn emit_to(&self, token: ServiceToken, payload: Value, to: &str) { self.emit_raw(token.name, payload, Some(to)); }
    pub fn emit_raw(&self, service: &str, payload: Value, to: Option<&str>) { self.send(Frame::Event { service: service.to_owned(), payload, to: to.map(str::to_owned) }); }
    pub fn on(&self, token: ServiceToken, handler: impl Fn(&Value) + Send + Sync + 'static) {
        self.on_event(move |name, payload, _| { if name == token.name { handler(payload); } });
    }
    pub fn on_event(&self, handler: impl Fn(&str, &Value, Option<&str>) + Send + Sync + 'static) { self.state.lock().expect("RPC state lock").events.push(Arc::new(handler)); }
    pub fn on_close(&self, handler: impl Fn() + Send + Sync + 'static) { self.state.lock().expect("RPC state lock").close_handlers.push(Arc::new(handler)); }
    pub fn close(&self) { self.close.abort(None); }
    fn send(&self, frame: Frame) { if self.outgoing.send(frame).is_err() { self.close(); } }
    pub async fn call(&self, method: &str, args: Vec<Value>) -> Result<Value, String> { self.call_with(CallOptions::default(), method, args).await }
    pub async fn call_with(&self, options: CallOptions, method: &str, args: Vec<Value>) -> Result<Value, String> {
        if options.signal.as_ref().is_some_and(AbortSignal::aborted) { return Err("Call cancelled".to_owned()); }
        let (sender, receiver) = oneshot::channel();
        let id = {
            let mut state = self.state.lock().expect("RPC state lock");
            if state.closed { return Err("Connection closed".to_owned()); }
            let id = state.next_id; state.next_id += 1; state.pending.insert(id, sender); id
        };
        self.send(Frame::Call { id, method: method.to_owned(), args });
        let timeout = async { match options.timeout_ms { Some(ms) => tokio::time::sleep(Duration::from_millis(ms)).await, None => std::future::pending().await } };
        let abort = async { match &options.signal { Some(signal) => signal.cancelled().await, None => std::future::pending().await } };
        let result = tokio::select! {
            result = receiver => return result.unwrap_or_else(|_| Err("Connection closed".to_owned())),
            _ = abort => Err("Call cancelled".to_owned()),
            _ = timeout => Err(format!("{method} timed out after {}ms", options.timeout_ms.unwrap_or_default())),
        };
        if self.state.lock().expect("RPC state lock").pending.remove(&id).is_some() { self.send(Frame::Cancel { id }); }
        result
    }
}
impl Drop for RpcPeer { fn drop(&mut self) { self.close(); } }

pub fn create_peer<R, W>(input: R, output: W, options: PeerOptions) -> RpcPeer
where R: AsyncRead + Unpin + Send + 'static, W: AsyncWrite + Unpin + Send + 'static {
    let state = Arc::new(Mutex::new(State { services: HashMap::new(), provided: Vec::new(), announced: HashSet::new(), pending: HashMap::new(), inflight: HashMap::new(), events: Vec::new(), close_handlers: Vec::new(), next_id: 1, closed: false }));
    let (outgoing, mut messages) = mpsc::unbounded_channel::<Frame>();
    let close = AbortController::new();
    let signal = close.signal();
    let write_signal = signal.clone();
    let writer_close = close.clone();
    tokio::spawn(async move {
        let mut output = output;
        loop {
            let frame = tokio::select! { biased; _ = write_signal.cancelled() => break, frame = messages.recv() => match frame { Some(frame) => frame, None => break } };
            let line = match serde_json::to_vec(&frame) { Ok(mut line) => { line.push(b'\n'); line }, Err(_) => break };
            let written = tokio::select! { biased; _ = write_signal.cancelled() => break, written = output.write_all(&line) => written };
            if written.is_err() { break; }
        }
        writer_close.abort(None);
    });
    let reader_state = state.clone();
    let reader_outgoing = outgoing.clone();
    let reader_close = close.clone();
    tokio::spawn(async move {
        let mut connection = JsonConnection::new(input, tokio::io::sink());
        let mut last_frame = tokio::time::Instant::now();
        let mut tick = tokio::time::interval(Duration::from_millis((options.dead_ms / 3).max(1)));
        tick.tick().await;
        loop {
            let value = tokio::select! {
                biased;
                _ = signal.cancelled() => break,
                _ = tick.tick(), if options.dead_ms > 0 => {
                    if last_frame.elapsed() > Duration::from_millis(options.dead_ms) { break; }
                    if reader_outgoing.send(Frame::Ping).is_err() { break; }
                    continue;
                }
                value = connection.receive() => match value { Ok(Some(value)) => value, _ => break },
            };
            last_frame = tokio::time::Instant::now();
            let frame: Frame = match serde_json::from_value(value) { Ok(frame) => frame, Err(_) => break };
            match frame {
                Frame::Event { service, payload, to } => {
                    let handlers = reader_state.lock().expect("RPC state lock").events.clone();
                    for handler in handlers { handler(&service, &payload, to.as_deref()); }
                }
                Frame::Announce { services } => { reader_state.lock().expect("RPC state lock").announced = services.into_iter().collect(); }
                Frame::Ping => {},
                Frame::Result { id, result } => {
                    if let Some(waiter) = reader_state.lock().expect("RPC state lock").pending.remove(&id) { let _ = waiter.send(Ok(result)); }
                }
                Frame::Error { id, error } => {
                    if let Some(waiter) = reader_state.lock().expect("RPC state lock").pending.remove(&id) { let _ = waiter.send(Err(error)); }
                }
                Frame::Cancel { id } => {
                    if let Some(controller) = reader_state.lock().expect("RPC state lock").inflight.remove(&id) { controller.abort(Some(AbortReason::new("Error", "Cancelled by caller"))); }
                }
                Frame::Call { id, method, args } => {
                    let controller = AbortController::new();
                    let (local, handler) = {
                        let mut state = reader_state.lock().expect("RPC state lock");
                        state.inflight.insert(id, controller.clone());
                        let service = method.split_once('.').and_then(|(name, _)| state.services.get(name));
                        let handler = method.split_once('.').and_then(|(_, name)| service.and_then(|service| service.get(name))).cloned();
                        (service.is_some(), handler)
                    };
                    let forward = options.forward.clone(); let outgoing = reader_outgoing.clone(); let state = reader_state.clone();
                    tokio::spawn(async move {
                        let result = match handler {
                            Some(handler) => handler(args, controller.signal()).await,
                            None if local => Err(format!("Unknown method: {method}")),
                            None => match forward { Some(forward) => forward(method, args).await, None => Err(format!("No service provides {method}")) },
                        };
                        let frame = match result { Ok(result) => Frame::Result { id, result }, Err(error) => Frame::Error { id, error } };
                        let _ = outgoing.send(frame);
                        state.lock().expect("RPC state lock").inflight.remove(&id);
                    });
                }
            }
        }
        reader_close.abort(None);
        let (pending, inflight, handlers) = {
            let mut state = reader_state.lock().expect("RPC state lock"); state.closed = true;
            (std::mem::take(&mut state.pending), std::mem::take(&mut state.inflight), state.close_handlers.clone())
        };
        for (_, waiter) in pending { let _ = waiter.send(Err("Connection closed".to_owned())); }
        for (_, controller) in inflight { controller.abort(Some(AbortReason::new("Error", "Connection closed"))); }
        for handler in handlers { handler(); }
    });
    RpcPeer { state, outgoing, close }
}
