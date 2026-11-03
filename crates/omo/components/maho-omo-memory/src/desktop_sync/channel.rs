//! Port of `desktop-sync/channel.ts`: `attachDesktopSyncChannel(stream)`.
//!
//! Registration owns the supplied stream. No endpoint lookup or alternate account source exists here.
//! The stream is any tokio duplex (a Unix socket from the desktop, a loopback pair in tests).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, watch};

use super::framing::{FrameReader, MAX_FRAME_BYTES, encode_frame};
use super::protocol::{
    DesktopSyncEvent, DesktopSyncReply, DesktopSyncRequest, DesktopSyncUnavailable, parse_child,
};

pub use super::protocol::{DesktopSyncEvent as Event, DesktopSyncRequest as Request};

const MAX_IN_FLIGHT: usize = 64;
const MAX_REQUEST_ID: u32 = 0x7fff_ffff;
const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(10);

/// Why no channel exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopSyncUnavailableReason {
    PrivateChannelUnavailable,
    PlatformNotSupported,
}

impl DesktopSyncUnavailableReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PrivateChannelUnavailable => "private_channel_unavailable",
            Self::PlatformNotSupported => "platform_not_supported",
        }
    }
}

pub enum DesktopSyncAttachment {
    Connected(DesktopSyncChannel),
    Unavailable(DesktopSyncUnavailableReason),
}

type Listener = Arc<dyn Fn(&DesktopSyncEvent) + Send + Sync>;
type Reply = Result<Value, DesktopSyncUnavailable>;

#[derive(Default)]
struct State {
    next_id: u32,
    child: Option<String>,
    waiting: HashMap<u32, oneshot::Sender<Reply>>,
    listeners: Vec<(u64, Listener)>,
    next_listener: u64,
}

struct Inner {
    state: Mutex<State>,
    writer: mpsc::UnboundedSender<Vec<u8>>,
    /// `true` once the channel closed (the upstream `signal`).
    lifetime: watch::Sender<bool>,
}

impl Inner {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn is_closed(&self) -> bool {
        *self.lifetime.borrow()
    }

    /// Idempotent: aborts the lifetime, fails every waiting request and drops every listener.
    fn close(&self) {
        if self.lifetime.send_replace(true) {
            return;
        }
        let waiting = {
            let mut state = self.state();
            state.listeners.clear();
            std::mem::take(&mut state.waiting)
        };
        for (_, pending) in waiting {
            let _ = pending.send(Err(DesktopSyncUnavailable));
        }
    }

    /// Not async: the authority's admission callback must enclose the immediate write.
    fn request(&self, packet: Value) -> oneshot::Receiver<Reply> {
        let (resolve, receive) = oneshot::channel();
        let mut state = self.state();
        if self.is_closed()
            || state.waiting.len() >= MAX_IN_FLIGHT
            || state.next_id >= MAX_REQUEST_ID
        {
            let _ = resolve.send(Err(DesktopSyncUnavailable));
            return receive;
        }
        state.next_id += 1;
        let id = state.next_id;
        let mut framed = json!({ "id": id });
        if let (Some(target), Some(source)) = (framed.as_object_mut(), packet.as_object()) {
            target.extend(source.clone());
        }
        state.waiting.insert(id, resolve);
        let written = encode_frame(&framed)
            .ok()
            .filter(|frame| frame.len() <= MAX_FRAME_BYTES + 1)
            .map(|frame| self.writer.send(frame).is_ok());
        drop(state);
        if written != Some(true) {
            self.close();
        }
        receive
    }

    fn receive(&self, packet: &Value) -> Result<bool, DesktopSyncUnavailable> {
        if packet
            .as_object()
            .is_some_and(|object| object.contains_key("event"))
        {
            let event = DesktopSyncEvent::parse(packet)?;
            let listeners: Vec<Listener> = {
                let state = self.state();
                if state.child.as_deref() != Some(event.child()) {
                    return Err(DesktopSyncUnavailable);
                }
                state
                    .listeners
                    .iter()
                    .map(|(_, listener)| listener.clone())
                    .collect()
            };
            for listener in listeners {
                listener(&event);
            }
            if matches!(event, DesktopSyncEvent::Retired { .. }) {
                self.close();
                return Ok(false);
            }
            return Ok(true);
        }
        let reply = DesktopSyncReply::parse(packet)?;
        let (id, outcome) = match reply {
            DesktopSyncReply::Ok { id, value } => (id, Ok(value)),
            DesktopSyncReply::Refused { id } => (id, Err(DesktopSyncUnavailable)),
        };
        let pending = self
            .state()
            .waiting
            .remove(&id)
            .ok_or(DesktopSyncUnavailable)?;
        let _ = pending.send(outcome);
        Ok(!self.is_closed())
    }
}

/// A connected channel. Dropping it does not close the stream; call [`DesktopSyncChannel::close`].
#[derive(Clone)]
pub struct DesktopSyncChannel {
    inner: Arc<Inner>,
}

/// Unsubscribes the listener when called.
pub struct Unsubscribe(Box<dyn FnOnce() + Send>);

impl Unsubscribe {
    pub fn call(self) {
        (self.0)();
    }
}

impl DesktopSyncChannel {
    /// Resolves once the channel closed (the upstream `signal` aborting).
    pub async fn closed(&self) {
        let mut receiver = self.inner.lifetime.subscribe();
        while !*receiver.borrow_and_update() {
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }

    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    pub fn close(&self) {
        self.inner.close();
    }

    pub fn on_event(
        &self,
        listener: impl Fn(&DesktopSyncEvent) + Send + Sync + 'static,
    ) -> Unsubscribe {
        let mut state = self.inner.state();
        let id = state.next_listener;
        state.next_listener += 1;
        if !self.inner.is_closed() {
            state.listeners.push((id, Arc::new(listener)));
        }
        let inner = self.inner.clone();
        Unsubscribe(Box::new(move || {
            inner
                .state()
                .listeners
                .retain(|(existing, _)| *existing != id)
        }))
    }

    /// Sends one typed operation under this channel's bootstrap capability. The write happens before
    /// this returns; only the reply is awaited.
    pub fn request(
        &self,
        request: &DesktopSyncRequest,
    ) -> impl std::future::Future<Output = Reply> + Send + use<> {
        // `request` re-parses like upstream's `safeParse`: only a well-formed operation is sent.
        let parsed = DesktopSyncRequest::parse(&request.to_value());
        let child = self.inner.state().child.clone();
        let receive = match (parsed, child) {
            (Ok(parsed), Some(child)) => Some(self.inner.request(
                json!({ "op": "request", "capability": child, "request": parsed.to_value() }),
            )),
            _ => None,
        };
        async move {
            match receive {
                Some(receive) => receive.await.unwrap_or(Err(DesktopSyncUnavailable)),
                None => Err(DesktopSyncUnavailable),
            }
        }
    }
}

/// `attachDesktopSyncChannel`: win32 -> `platform_not_supported`, no stream ->
/// `private_channel_unavailable`, otherwise a bootstrap handshake bounded by 10 s.
pub async fn attach_desktop_sync_channel<S>(stream: Option<S>) -> DesktopSyncAttachment
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    if cfg!(windows) {
        drop(stream);
        return DesktopSyncAttachment::Unavailable(
            DesktopSyncUnavailableReason::PlatformNotSupported,
        );
    }
    let Some(stream) = stream else {
        return DesktopSyncAttachment::Unavailable(
            DesktopSyncUnavailableReason::PrivateChannelUnavailable,
        );
    };
    let (mut read_half, mut write_half) = tokio::io::split(stream);
    let (writer, mut outgoing) = mpsc::unbounded_channel::<Vec<u8>>();
    let inner = Arc::new(Inner {
        state: Mutex::new(State::default()),
        writer,
        lifetime: watch::channel(false).0,
    });

    let writing = inner.clone();
    tokio::spawn(async move {
        let mut closed = writing.lifetime.subscribe();
        loop {
            tokio::select! {
                frame = outgoing.recv() => {
                    let Some(frame) = frame else { return };
                    if write_half.write_all(&frame).await.is_err() {
                        writing.close();
                        return;
                    }
                }
                _ = async { let _ = closed.wait_for(|closed| *closed).await; } => {
                    let _ = write_half.shutdown().await;
                    return;
                }
            }
        }
    });

    let reading = inner.clone();
    tokio::spawn(async move {
        let mut closed = reading.lifetime.subscribe();
        let mut frames = FrameReader::default();
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            tokio::select! {
                read = read_half.read(&mut buffer) => {
                    let Ok(count) = read else { break };
                    if count == 0 {
                        break;
                    }
                    let mut failed = false;
                    let pushed = frames.push(&buffer[..count], |packet| match reading.receive(&packet) {
                        Ok(more) => more,
                        Err(_) => {
                            failed = true;
                            false
                        }
                    });
                    if pushed.is_err() || failed {
                        break;
                    }
                }
                _ = closed.wait_for(|closed| *closed) => return,
            }
        }
        reading.close();
    });

    let bootstrap = inner.request(json!({ "op": "bootstrap" }));
    let outcome = tokio::time::timeout(BOOTSTRAP_TIMEOUT, bootstrap).await;
    let child = match outcome {
        Ok(Ok(Ok(value))) => parse_child(&value).ok(),
        _ => None,
    };
    match child {
        Some(child) if !inner.is_closed() => {
            inner.state().child = Some(child);
            DesktopSyncAttachment::Connected(DesktopSyncChannel { inner })
        }
        _ => {
            inner.close();
            DesktopSyncAttachment::Unavailable(
                DesktopSyncUnavailableReason::PrivateChannelUnavailable,
            )
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use tokio::io::{AsyncBufReadExt, BufReader};

    use super::*;

    const BOUND: Duration = Duration::from_secs(20);

    async fn answer(
        peer: tokio::io::DuplexStream,
        replies: impl Fn(&Value) -> Option<Value> + Send + 'static,
    ) -> tokio::task::JoinHandle<Vec<Value>> {
        tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(peer);
            let mut lines = BufReader::new(read).lines();
            let mut packets = Vec::new();
            while let Ok(Some(line)) = lines.next_line().await {
                let packet: Value = serde_json::from_str(&line).expect("valid fixture packet");
                if let Some(reply) = replies(&packet) {
                    write
                        .write_all(format!("{reply}\n").as_bytes())
                        .await
                        .expect("write fixture reply");
                }
                packets.push(packet);
                if packets.len() == 2 {
                    break;
                }
            }
            packets
        })
    }

    #[tokio::test]
    async fn a_missing_stream_is_unavailable() {
        let none: Option<tokio::io::DuplexStream> = None;
        assert!(matches!(
            attach_desktop_sync_channel(none).await,
            DesktopSyncAttachment::Unavailable(
                DesktopSyncUnavailableReason::PrivateChannelUnavailable
            )
        ));
    }

    #[tokio::test]
    async fn bootstraps_once_and_sends_requests_under_its_capability() {
        let (engine, peer) = tokio::io::duplex(64 * 1024);
        let peer = answer(peer, |packet| {
            let id = packet["id"].clone();
            Some(if packet["op"] == "bootstrap" {
                json!({"id": id, "ok": true, "value": {"child": "test-child"}})
            } else {
                json!({"id": id, "ok": true, "value": {"accepted": true}})
            })
        })
        .await;
        let DesktopSyncAttachment::Connected(channel) =
            tokio::time::timeout(BOUND, attach_desktop_sync_channel(Some(engine)))
                .await
                .unwrap()
        else {
            panic!("channel unavailable");
        };
        let response =
            tokio::time::timeout(BOUND, channel.request(&DesktopSyncRequest::OperationBegin))
                .await
                .unwrap();
        assert_eq!(response, Ok(json!({"accepted": true})));
        assert_eq!(
            tokio::time::timeout(BOUND, peer).await.unwrap().unwrap(),
            vec![
                json!({"id": 1, "op": "bootstrap"}),
                json!({"id": 2, "op": "request", "capability": "test-child", "request": {"op": "operationBegin"}}),
            ]
        );
        channel.close();
        assert!(channel.is_closed());
        assert_eq!(
            channel.request(&DesktopSyncRequest::Descriptor).await,
            Err(DesktopSyncUnavailable)
        );
    }

    #[tokio::test]
    async fn an_event_from_another_child_closes_the_channel_and_a_retired_event_ends_it() {
        let (engine, peer) = tokio::io::duplex(64 * 1024);
        let (read, mut write) = tokio::io::split(peer);
        let mut lines = BufReader::new(read).lines();
        let attach = tokio::spawn(attach_desktop_sync_channel(Some(engine)));
        let bootstrap: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        write
            .write_all(
                format!(
                    "{}\n",
                    json!({"id": bootstrap["id"], "ok": true, "value": {"child": "kid"}})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let DesktopSyncAttachment::Connected(channel) =
            tokio::time::timeout(BOUND, attach).await.unwrap().unwrap()
        else {
            panic!("channel unavailable");
        };
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        channel.on_event(move |event| sink.lock().unwrap().push(event.clone()));
        write
            .write_all(
                format!(
                    "{}\n",
                    json!({"event": "connected", "child": "kid", "handle": "h", "connected": true})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        write
            .write_all(format!("{}\n", json!({"event": "retired", "child": "kid"})).as_bytes())
            .await
            .unwrap();
        tokio::time::timeout(BOUND, channel.closed()).await.unwrap();
        assert_eq!(seen.lock().unwrap().len(), 2);

        let (engine, peer) = tokio::io::duplex(64 * 1024);
        let (read, mut write) = tokio::io::split(peer);
        let mut lines = BufReader::new(read).lines();
        let attach = tokio::spawn(attach_desktop_sync_channel(Some(engine)));
        let bootstrap: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        write
            .write_all(
                format!(
                    "{}\n",
                    json!({"id": bootstrap["id"], "ok": true, "value": {"child": "kid"}})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let DesktopSyncAttachment::Connected(channel) =
            tokio::time::timeout(BOUND, attach).await.unwrap().unwrap()
        else {
            panic!("channel unavailable");
        };
        write
            .write_all(format!("{}\n", json!({"event": "retired", "child": "stranger"})).as_bytes())
            .await
            .unwrap();
        tokio::time::timeout(BOUND, channel.closed()).await.unwrap();
    }
}
