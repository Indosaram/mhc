//! Private presenter framing (senpi `mcp-presenter-wire.ts`): signed proofs, bounded JSONL frames, and a
//! channel that is destroyed on any malformed, oversized or late frame.
use serde_json::Value;
use std::sync::{Arc, Mutex};

/// One private record may not exceed this many UTF-16 units (senpi `maxLineLength: 16 * 1024`).
pub const PRIVATE_FRAME_LIMIT: usize = 16 * 1024;
/// A private channel that stays silent this long is destroyed (senpi `setTimeout(..., 30_000)`).
pub const PRIVATE_FRAME_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// Any byte stream a private channel can own: a socket, the inherited bootstrap descriptor, or an in-memory pair.
pub trait PrivateStream:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static
{
}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static> PrivateStream for T {}
pub type BoxedPrivateStream = Box<dyn PrivateStream>;

/// senpi `signed`: exactly `{type:"proof",signature:<128 hex>}`, verified over the transcript bytes.
pub fn signed(value: &Value, transcript: &str, key: &ed25519_dalek::VerifyingKey) -> bool {
    let Some(record) = value.as_object() else {
        return false;
    };
    let Some(signature) = record.get("signature").and_then(Value::as_str) else {
        return false;
    };
    if record.len() != 2
        || record.get("type").and_then(Value::as_str) != Some("proof")
        || signature.len() != 128
        || !signature
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return false;
    }
    let Ok(bytes) = hex::decode(signature) else {
        return false;
    };
    let Ok(signature) = ed25519_dalek::Signature::from_slice(&bytes) else {
        return false;
    };
    key.verify_strict(transcript.as_bytes(), &signature).is_ok()
}

enum Outbound {
    Line(String),
    End,
}

struct ChannelState {
    destroyed: bool,
    detached: bool,
    deadline: Option<tokio::task::JoinHandle<()>>,
    on_close: Vec<Box<dyn FnOnce() + Send>>,
}

/// One owned private stream: a reader that hands every parsed frame to `receive`, and an ordered writer.
///
/// senpi uses the Node stream directly; `destroy()` here aborts both pumps and drops the stream, and every
/// `once("close")` callback runs exactly once.
#[derive(Clone)]
pub struct PrivateChannel {
    outbox: tokio::sync::mpsc::UnboundedSender<Outbound>,
    state: Arc<Mutex<ChannelState>>,
    tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    id: u64,
}
static NEXT_CHANNEL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl PartialEq for PrivateChannel {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for PrivateChannel {}
impl std::fmt::Debug for PrivateChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrivateChannel")
            .field("id", &self.id)
            .finish()
    }
}

impl PrivateChannel {
    /// senpi `privateFrames(stream, receive, deadline)` plus `send`: bound before parsing, and neither a malformed
    /// body nor an oversized line reaches `receive`.
    pub fn open(
        stream: BoxedPrivateStream,
        receive: impl FnMut(&PrivateChannel, Value) + Send + 'static,
        deadline: bool,
    ) -> Self {
        let (outbox, mut outbound) = tokio::sync::mpsc::unbounded_channel::<Outbound>();
        let channel = Self {
            outbox,
            state: Arc::new(Mutex::new(ChannelState {
                destroyed: false,
                detached: false,
                deadline: None,
                on_close: Vec::new(),
            })),
            tasks: Arc::default(),
            id: NEXT_CHANNEL.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        };
        let (mut read, mut write) = tokio::io::split(stream);
        let writer = channel.clone();
        let write_task = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            while let Some(message) = outbound.recv().await {
                match message {
                    Outbound::Line(line) => {
                        if write.write_all(line.as_bytes()).await.is_err()
                            || write.flush().await.is_err()
                        {
                            writer.destroy();
                            return;
                        }
                    }
                    Outbound::End => {
                        let _ = write.shutdown().await;
                        return;
                    }
                }
            }
        });
        let reader = channel.clone();
        let mut receive = receive;
        let read_task = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let Ok(mut lines) = crate::jsonl::JsonlLineReader::new(PRIVATE_FRAME_LIMIT) else {
                return;
            };
            let mut chunk = [0u8; 4096];
            loop {
                let count = match read.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(count) => count,
                };
                for record in lines.push(&chunk[..count]) {
                    if !reader.deliver(record, &mut receive) {
                        return;
                    }
                }
            }
            for record in lines.finish() {
                if !reader.deliver(record, &mut receive) {
                    return;
                }
            }
            // Peer EOF is a close: nothing more can arrive on a private channel.
            reader.destroy();
        });
        lock(&channel.tasks).extend([write_task, read_task]);
        if deadline {
            let expired = channel.clone();
            lock(&channel.state).deadline = Some(tokio::spawn(async move {
                tokio::time::sleep(PRIVATE_FRAME_DEADLINE).await;
                expired.destroy();
            }));
        }
        channel
    }

    fn deliver(
        &self,
        record: crate::jsonl::LineRecord,
        receive: &mut (impl FnMut(&PrivateChannel, Value) + Send),
    ) -> bool {
        if lock(&self.state).destroyed {
            return false;
        }
        if lock(&self.state).detached {
            return true;
        }
        match record {
            crate::jsonl::LineRecord::Oversized => {
                self.destroy();
                false
            }
            crate::jsonl::LineRecord::Line(line) => match serde_json::from_str::<Value>(&line) {
                Ok(value) => {
                    receive(self, value);
                    !lock(&self.state).destroyed
                }
                Err(_) => {
                    self.destroy();
                    false
                }
            },
        }
    }

    /// senpi `send`: one JSONL record; a failed serialization destroys the channel.
    pub fn send(&self, value: &Value) -> bool {
        if lock(&self.state).destroyed {
            return false;
        }
        match crate::jsonl::serialize_json_line(value) {
            Ok(line) => match self.outbox.send(Outbound::Line(line)) {
                Ok(()) => true,
                Err(_) => {
                    self.destroy();
                    false
                }
            },
            _ => {
                self.destroy();
                false
            }
        }
    }

    /// `stream.end(line)`: the final record, then the write side closes.
    pub fn end(&self, value: &Value) {
        if self.send(value) {
            let _ = self.outbox.send(Outbound::End);
        }
    }

    /// The returned detach of `privateFrames`: stop reading and clear the deadline, keeping the stream.
    pub fn stop(&self) {
        let mut state = lock(&self.state);
        state.detached = true;
        if let Some(deadline) = state.deadline.take() {
            deadline.abort();
        }
    }

    pub fn destroy(&self) {
        let callbacks = {
            let mut state = lock(&self.state);
            if state.destroyed {
                return;
            }
            state.destroyed = true;
            if let Some(deadline) = state.deadline.take() {
                deadline.abort();
            }
            std::mem::take(&mut state.on_close)
        };
        for task in lock(&self.tasks).drain(..) {
            task.abort();
        }
        for callback in callbacks {
            callback();
        }
    }

    pub fn is_destroyed(&self) -> bool {
        lock(&self.state).destroyed
    }

    /// `stream.once("close", callback)`; runs at once on an already closed channel.
    pub fn on_close(&self, callback: impl FnOnce() + Send + 'static) {
        let mut state = lock(&self.state);
        if state.destroyed {
            drop(state);
            callback();
        } else {
            state.on_close.push(Box::new(callback));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;
    use serde_json::json;

    #[test]
    fn signed_accepts_only_the_exact_proof_shape() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let signature = hex::encode(key.sign(b"transcript").to_bytes());
        let public = key.verifying_key();
        assert!(signed(
            &json!({"type":"proof","signature":signature}),
            "transcript",
            &public
        ));
        assert!(!signed(
            &json!({"type":"proof","signature":signature}),
            "other",
            &public
        ));
        assert!(!signed(
            &json!({"type":"proof","signature":signature,"extra":1}),
            "transcript",
            &public
        ));
        assert!(!signed(
            &json!({"type":"proof","signature":signature.to_uppercase()}),
            "transcript",
            &public
        ));
    }

    #[tokio::test]
    async fn malformed_and_oversized_frames_destroy_the_channel() {
        use tokio::io::AsyncWriteExt;
        let (mut peer, host) = tokio::io::duplex(1 << 16);
        let (frames, mut received) = tokio::sync::mpsc::unbounded_channel();
        let (closed_tx, closed) = tokio::sync::oneshot::channel();
        let channel = PrivateChannel::open(
            Box::new(host),
            move |_, value| {
                let _ = frames.send(value);
            },
            false,
        );
        channel.on_close(move || {
            let _ = closed_tx.send(());
        });
        peer.write_all(b"{\"a\":1}\nnot json\n{\"b\":2}\n")
            .await
            .unwrap();
        closed.await.unwrap();
        assert_eq!(received.recv().await, Some(json!({"a":1})));
        assert!(received.try_recv().is_err());
        assert!(channel.is_destroyed());
        assert!(!channel.send(&json!({})));

        let (mut peer, host) = tokio::io::duplex(1 << 16);
        let (closed_tx, closed) = tokio::sync::oneshot::channel();
        let channel = PrivateChannel::open(Box::new(host), |_, _| {}, false);
        channel.on_close(move || {
            let _ = closed_tx.send(());
        });
        peer.write_all("x".repeat(PRIVATE_FRAME_LIMIT + 1).as_bytes())
            .await
            .unwrap();
        closed.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_channel_is_destroyed_at_its_deadline() {
        let (_peer, host) = tokio::io::duplex(64);
        let (closed_tx, closed) = tokio::sync::oneshot::channel();
        let channel = PrivateChannel::open(Box::new(host), |_, _| {}, true);
        channel.on_close(move || {
            let _ = closed_tx.send(());
        });
        closed.await.unwrap();
        assert!(channel.is_destroyed());
    }
}
