//! Persistent authenticated connection to the engine host for cold wake (`thread/wake/carrier-connection.ts`).

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Notify, mpsc, oneshot};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

const REQUEST_TIMEOUT_MS: u64 = 30_000;

pub trait CarrierHostConnection: Send + Sync {
    fn request(&self, frame: Map<String, Value>) -> BoxFuture<Result<Value, String>>;
    fn subscribe_events(&self) -> mpsc::UnboundedReceiver<Value>;
    fn closed_notify(&self) -> Arc<Notify>;
    fn is_closed(&self) -> bool;
    fn close(&self);
}

#[cfg(unix)]
type PendingRequestMap = HashMap<String, oneshot::Sender<Result<Value, String>>>;

#[cfg(unix)]
pub struct UnixCarrierHostConnection {
    tx: mpsc::UnboundedSender<String>,
    pending: Arc<Mutex<PendingRequestMap>>,
    listeners: Arc<Mutex<Vec<mpsc::UnboundedSender<Value>>>>,
    closed: Arc<Notify>,
    is_closed_flag: Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(unix)]
impl UnixCarrierHostConnection {
    pub async fn connect(socket_path: &str, key_root: Option<PathBuf>) -> Result<Self, String> {
        let probe = serde_json::json!({
            "type": "get_protocol_info",
            "observe": true,
        });
        let (rpc_socket, _) = maho_rpc::host_probe::authenticated_host_socket(
            socket_path,
            &probe,
            REQUEST_TIMEOUT_MS,
            key_root.as_deref(),
        )
        .await
        .map_err(|e| format!("host_unavailable:{socket_path}: {e}"))?;

        let stream = rpc_socket
            .into_stream()
            .map_err(|e| format!("host_unavailable:{socket_path}: {e}"))?;
        let (read_half, mut write_half) = stream.into_split();

        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let pending: Arc<Mutex<PendingRequestMap>> = Arc::new(Mutex::new(HashMap::new()));
        let listeners: Arc<Mutex<Vec<mpsc::UnboundedSender<Value>>>> =
            Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(Notify::new());
        let is_closed_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let pending_w = pending.clone();
        let closed_w = closed.clone();
        let flag_w = is_closed_flag.clone();
        tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                if write_half.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
            flag_w.store(true, std::sync::atomic::Ordering::Release);
            let mut guard = pending_w
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for (_, sender) in guard.drain() {
                let _ = sender.send(Err("carrier connection writer closed".to_owned()));
            }
            closed_w.notify_waiters();
        });

        let pending_r = pending.clone();
        let listeners_r = listeners.clone();
        let closed_r = closed.clone();
        let flag_r = is_closed_flag.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        let Ok(frame) = serde_json::from_str::<Value>(trimmed) else {
                            continue;
                        };
                        let id = frame.get("id").and_then(Value::as_str);
                        let is_response =
                            frame.get("type").and_then(Value::as_str) == Some("response");
                        if is_response
                            && let Some(req_id) = id
                            && let Some(tx_reply) = pending_r
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .remove(req_id)
                        {
                            let _ = tx_reply.send(Ok(frame));
                            continue;
                        }
                        let mut guard = listeners_r
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        guard.retain(|l| l.send(frame.clone()).is_ok());
                    }
                }
            }
            flag_r.store(true, std::sync::atomic::Ordering::Release);
            let mut guard = pending_r
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for (_, sender) in guard.drain() {
                let _ = sender.send(Err(
                    "the engine host closed the carrier's connection".to_owned()
                ));
            }
            closed_r.notify_waiters();
        });

        Ok(Self {
            tx,
            pending,
            listeners,
            closed,
            is_closed_flag,
        })
    }
}

#[cfg(unix)]
impl CarrierHostConnection for UnixCarrierHostConnection {
    fn request(&self, mut frame: Map<String, Value>) -> BoxFuture<Result<Value, String>> {
        let tx = self.tx.clone();
        let pending = self.pending.clone();
        let is_closed = self
            .is_closed_flag
            .load(std::sync::atomic::Ordering::Acquire);
        Box::pin(async move {
            if is_closed {
                return Err("the engine host closed the carrier's connection".to_owned());
            }
            let id = uuid::Uuid::new_v4().to_string();
            let cmd_type = frame
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("request")
                .to_owned();
            frame.insert("id".to_owned(), Value::String(id.clone()));
            let line = format!(
                "{}\n",
                serde_json::to_string(&Value::Object(frame)).map_err(|e| e.to_string())?
            );
            let (reply_tx, reply_rx) = oneshot::channel();
            pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(id.clone(), reply_tx);
            if tx.send(line).is_err() {
                pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&id);
                return Err("carrier connection closed before send".to_owned());
            }
            match tokio::time::timeout(Duration::from_millis(REQUEST_TIMEOUT_MS), reply_rx).await {
                Ok(Ok(res)) => res,
                Ok(Err(_)) => Err("the engine host closed the carrier's connection".to_owned()),
                Err(_) => {
                    pending
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&id);
                    Err(format!("the engine host did not answer {cmd_type} in time"))
                }
            }
        })
    }

    fn subscribe_events(&self) -> mpsc::UnboundedReceiver<Value> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(tx);
        rx
    }

    fn closed_notify(&self) -> Arc<Notify> {
        self.closed.clone()
    }

    fn is_closed(&self) -> bool {
        self.is_closed_flag
            .load(std::sync::atomic::Ordering::Acquire)
    }

    fn close(&self) {
        self.is_closed_flag
            .store(true, std::sync::atomic::Ordering::Release);
        self.closed.notify_waiters();
    }
}
