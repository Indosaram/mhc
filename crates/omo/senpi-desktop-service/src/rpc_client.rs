//! Id-multiplexed NDJSON JSON-RPC client over one engine child.
//! Matches `src/service/rpc-client.ts`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{oneshot, Mutex};
use tokio::time::sleep;

use senpi_desktop_protocol::JsonRpcErrorData;
use serde_json::{json, Value};

use crate::child::EngineChild;
use crate::parse::{is_record, parse_error_data};
use crate::timeouts::{GRACE_TIMEOUT, RESTART_MESSAGE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopServiceErrorCode {
    Timeout,
    Cancelled,
    Closed,
    Internal,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[error("{message}")]
pub struct DesktopServiceError {
    pub code: DesktopServiceErrorCode,
    pub message: String,
    pub engine_restarted: bool,
}

impl DesktopServiceError {
    pub fn new(code: DesktopServiceErrorCode, message: impl Into<String>, engine_restarted: bool) -> Self {
        Self {
            code,
            message: message.into(),
            engine_restarted,
        }
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(DesktopServiceErrorCode::Timeout, message, false)
    }

    pub fn cancelled(message: impl Into<String>) -> Self {
        Self::new(DesktopServiceErrorCode::Cancelled, message, false)
    }

    pub fn closed(message: impl Into<String>) -> Self {
        Self::new(DesktopServiceErrorCode::Closed, message, false)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(DesktopServiceErrorCode::Internal, message, false)
    }

    pub fn code(&self) -> DesktopServiceErrorCode {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, thiserror::Error, Clone, PartialEq)]
#[error("{message}")]
pub struct DesktopEngineRpcError {
    pub method: String,
    pub rpc_code: i64,
    pub message: String,
    pub data: Option<JsonRpcErrorData>,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[error("{message}")]
pub struct DesktopLaunchContextError {
    pub message: String,
    pub reason: String,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum DesktopError {
    #[error(transparent)]
    LaunchContext(#[from] DesktopLaunchContextError),
    #[error(transparent)]
    Service(#[from] DesktopServiceError),
    #[error(transparent)]
    Rpc(Box<DesktopEngineRpcError>),
    #[error(transparent)]
    AbiMismatch(Box<crate::service::DesktopEngineAbiMismatchError>),
}

#[derive(Default, Clone)]
pub struct CallOptions {
    pub timeout: Option<Duration>,
    pub cancel_rx: Option<Arc<Mutex<oneshot::Receiver<()>>>>,
}

pub struct RpcClientHooks {
    pub on_notification: Arc<dyn Fn(String, Value) + Send + Sync>,
    pub on_exit: Arc<dyn Fn() + Send + Sync>,
}

struct Pending {
    method: String,
    tx: oneshot::Sender<Result<Value, DesktopError>>,
    cancelled: Option<DesktopServiceError>,
}

pub struct RpcClient {
    child: Arc<EngineChild>,
    hooks: RpcClientHooks,
    pending: Arc<Mutex<HashMap<u64, Pending>>>,
    next_id: AtomicU64,
    exit_reason: Arc<Mutex<Option<String>>>,
    alive: Arc<AtomicBool>,
}

impl RpcClient {
    pub fn new(child: EngineChild, hooks: RpcClientHooks) -> Arc<Self> {
        let child_arc = Arc::new(child);
        let client = Arc::new(Self {
            child: Arc::clone(&child_arc),
            hooks,
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            exit_reason: Arc::new(Mutex::new(None)),
            alive: Arc::new(AtomicBool::new(true)),
        });

        let client_clone = Arc::clone(&client);
        let child_for_streams = Arc::clone(&child_arc);

        tokio::spawn(async move {
            let (line_rx, exit_rx) = child_for_streams.take_streams().await;

            if let Some(mut lines) = line_rx {
                while let Some(line) = lines.recv().await {
                    client_clone.handle_line(&line).await;
                }
            }

            if let Some(exit) = exit_rx
                && let Ok(reason) = exit.await {
                    client_clone.handle_exit(reason).await;
            }
        });

        client
    }

    pub fn engine_path(&self) -> &str {
        &self.child.path
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst) && self.child.is_alive()
    }

    pub async fn exited(&self) {
        self.child.wait_exited().await;
    }

    pub fn end(&self) {
        self.child.end();
    }

    pub fn kill(&self) {
        self.alive.store(false, Ordering::SeqCst);
        self.child.kill();
    }

    pub async fn request(
        self: &Arc<Self>,
        method: &str,
        params: Value,
        options: CallOptions,
    ) -> Result<Value, DesktopError> {
        if !self.is_alive() {
            let reason = self.exit_reason.lock().await.clone();
            return Err(DesktopError::Service(DesktopServiceError::closed(
                reason.unwrap_or_else(|| "desktop engine was killed".to_string()),
            )));
        }

        if let Some(cancel_rx) = options.cancel_rx.as_ref() {
            let mut cancel_rx = cancel_rx.lock().await;
            if cancel_rx.try_recv().is_ok() {
                return Err(DesktopError::Service(DesktopServiceError::cancelled(
                    format!("{method} was cancelled before it was sent"),
                )));
            }
        }

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, mut rx) = oneshot::channel();

        {
            let mut pending = self.pending.lock().await;
            pending.insert(
                id,
                Pending {
                    method: method.to_string(),
                    tx,
                    cancelled: None,
                },
            );
        }

        let wire_msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let line = format!("{}\n", wire_msg);
        if let Err(e) = self.child.write_line(&line) {
            let mut pending = self.pending.lock().await;
            pending.remove(&id);
            return Err(DesktopError::Service(e));
        }

        let timeout_dur = options.timeout;
        let cancel_rx = options.cancel_rx;
        let client_clone = Arc::clone(self);

        tokio::select! {
            res = &mut rx => {
                return match res {
                    Ok(r) => r,
                    Err(_) => {
                        let reason = client_clone.exit_reason.lock().await.clone();
                        Err(DesktopError::Service(DesktopServiceError::closed(
                            reason.unwrap_or_else(|| "desktop engine exited".to_string()),
                        )))
                    }
                };
            }
            _ = async {
                if let Some(rx_mutex) = cancel_rx {
                    let mut g = rx_mutex.lock().await;
                    let _ = (&mut *g).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                client_clone.cancel(id, DesktopServiceErrorCode::Cancelled, format!("{method} was cancelled")).await;
            }
            _ = async {
                if let Some(dur) = timeout_dur {
                    sleep(dur).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                client_clone.cancel(id, DesktopServiceErrorCode::Timeout, format!("{method} timed out")).await;
            }
        }
        rx.await.unwrap_or_else(|_| {
            Err(DesktopError::Service(DesktopServiceError::closed(
                "desktop engine exited during cancellation",
            )))
        })
    }

    async fn cancel(self: &Arc<Self>, id: u64, code: DesktopServiceErrorCode, message: String) {
        {
            let mut pending = self.pending.lock().await;
            if let Some(p) = pending.get_mut(&id) {
                if p.cancelled.is_some() {
                    return;
                }
                p.cancelled = Some(DesktopServiceError::new(code, message.clone(), false));
            } else {
                return;
            }
        }

        let cancel_msg = json!({
            "jsonrpc": "2.0",
            "method": "$/cancel",
            "params": { "id": id },
        });
        let _ = self.child.write_line(&format!("{}\n", cancel_msg));

        let client_clone = Arc::clone(self);
        tokio::spawn(async move {
            sleep(GRACE_TIMEOUT).await;
            let mut pending = client_clone.pending.lock().await;
            if let Some(p) = pending.remove(&id) {
                client_clone.alive.store(false, Ordering::SeqCst);
                client_clone.child.kill();
                let _ = p.tx.send(Err(DesktopError::Service(DesktopServiceError::new(
                    code,
                    format!("{message}; {RESTART_MESSAGE}"),
                    true,
                ))));
            }
        });
    }

    pub async fn handle_line(&self, line: &str) {
        let mut parsed: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                self.alive.store(false, Ordering::SeqCst);
                self.child.kill();
                let mut reason = self.exit_reason.lock().await;
                *reason = Some(format!(
                    "desktop engine wrote a non-protocol line \"{}\"",
                    line.chars().take(200).collect::<String>()
                ));
                return;
            }
        };

        if !is_record(&parsed) {
            self.alive.store(false, Ordering::SeqCst);
            self.child.kill();
            let mut reason = self.exit_reason.lock().await;
            *reason = Some(format!(
                "desktop engine wrote a non-protocol line \"{}\"",
                line.chars().take(200).collect::<String>()
            ));
            return;
        }

        let Some(obj) = parsed.as_object_mut() else {
            return;
        };
        if let Some(method_val) = obj.get("method")
            && obj.get("id").is_none() {
                if let Some(method) = method_val.as_str() {
                    let params = obj.get("params").cloned().unwrap_or(Value::Null);
                    (self.hooks.on_notification)(method.to_string(), params);
                }
                return;
        }

        let id = match obj.get("id").and_then(|v| v.as_u64()) {
            Some(id) => id,
            None => return,
        };

        let mut pending_guard = self.pending.lock().await;
        if let Some(pending) = pending_guard.remove(&id) {
            if let Some(err) = pending.cancelled {
                let _ = pending.tx.send(Err(DesktopError::Service(err)));
                return;
            }

            if let Some(result) = obj.remove("result") {
                let _ = pending.tx.send(Ok(result));
            } else if let Some(error) = obj.get("error").and_then(|e| e.as_object())
                && let Some(code) = error.get("code").and_then(Value::as_i64)
                && let Some(message) = error.get("message").and_then(Value::as_str)
            {
                let message = message.to_string();
                let data = error.get("data").and_then(parse_error_data);
                let _ = pending.tx.send(Err(DesktopError::Rpc(Box::new(DesktopEngineRpcError {
                    method: pending.method,
                    rpc_code: code,
                    message,
                    data,
                }))));
            } else {
                let _ = pending.tx.send(Err(DesktopError::Service(DesktopServiceError::internal(
                    format!("desktop engine sent a malformed reply to {}", pending.method),
                ))));
            }
        }
    }

    pub async fn handle_exit(&self, reason: String) {
        self.alive.store(false, Ordering::SeqCst);
        {
            let mut r = self.exit_reason.lock().await;
            if r.is_none() {
                *r = Some(reason.clone());
            }
        }
        let mut pending = self.pending.lock().await;
        let reason = self.exit_reason.lock().await.clone().unwrap_or(reason);
        let closed_err = DesktopServiceError::closed(reason);
        for (_, p) in pending.drain() {
            let _ = p.tx.send(Err(DesktopError::Service(closed_err.clone())));
        }
        (self.hooks.on_exit)();
    }
}
