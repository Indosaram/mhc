//! Desktop service coordinator matching `src/service/service.ts`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tokio::time::sleep;

use senpi_desktop_core::protocol::{ABI as ENGINE_ABI, PROTOCOL_VERSION};
use senpi_desktop_protocol::{AuditEvent, DesktopCapabilities, StopPathStatus};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::child::{engine_child_factory, ChildFactory};
use crate::notifications::{NotificationHub, Subscription};
use crate::parse::{parse_hello, parse_session_opened};
use crate::rpc_client::{
    CallOptions, DesktopError, DesktopServiceError, RpcClient, RpcClientHooks,
};
use crate::timeouts::{
    CAPABILITIES_TIMEOUT, CLOSE_TIMEOUT, HEARTBEAT_INTERVAL, START_TIMEOUT, START_TIMEOUT_MESSAGE,
};

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[error("desktop engine ABI mismatch at {engine_path}: expected abi {expected_abi} protocol {expected_protocol}, engine reported abi {actual_abi} protocol {actual_protocol}")]
pub struct DesktopEngineAbiMismatchError {
    pub engine_path: String,
    pub expected_abi: String,
    pub expected_protocol: String,
    pub actual_abi: String,
    pub actual_protocol: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotGcParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_interval_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureCapsParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinate_safe: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSessionOpenParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub macos_canary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_host_relay_only_stop: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot_gc: Option<ScreenshotGcParams>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_caps: Option<CaptureCapsParams>,
}

pub type NotificationObserver = Arc<dyn Fn(&str, &Value) + Send + Sync>;

#[derive(Default)]
pub struct DesktopServiceOptions {
    pub create_child: Option<ChildFactory>,
    pub on_notification: Option<NotificationObserver>,
}

struct Connection {
    rpc: Arc<RpcClient>,
    resume_token: Mutex<String>,
    armed_chord: Mutex<Option<String>>,
    heartbeat_active: Arc<AtomicBool>,
    heartbeat_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

#[derive(Clone)]
struct DesiredSession {
    params: DesktopSessionOpenParams,
    chord: Option<String>,
}

pub struct DesktopService {
    create_child: ChildFactory,
    hub: NotificationHub,
    lifecycle: Mutex<()>,
    connection: Arc<RwLock<Option<Arc<Connection>>>>,
    desired: RwLock<Option<DesiredSession>>,
    last_stop_path: Arc<std::sync::Mutex<Option<StopPathStatus>>>,
    on_notification: Option<NotificationObserver>,
}

impl DesktopService {
    pub fn new(options: DesktopServiceOptions) -> Self {
        let hub = NotificationHub::new();
        let last_stop_path = Arc::new(std::sync::Mutex::new(None));
        let lsp_clone = Arc::clone(&last_stop_path);
        hub.on_stop_path_change(move |status| {
            if let Ok(mut g) = lsp_clone.lock() {
                *g = Some(status);
            }
        });

        let create_child = options
            .create_child
            .unwrap_or_else(|| engine_child_factory(None));
        Self {
            create_child,
            hub,
            lifecycle: Mutex::new(()),
            connection: Arc::new(RwLock::new(None)),
            desired: RwLock::new(None),
            last_stop_path,
            on_notification: options.on_notification,
        }
    }

    pub fn on_audit(&self, listener: impl Fn(AuditEvent) + Send + Sync + 'static) -> Subscription {
        self.hub.on_audit(listener)
    }

    pub fn on_stop_path_change(
        &self,
        listener: impl Fn(StopPathStatus) + Send + Sync + 'static,
    ) -> Subscription {
        self.hub.on_stop_path_change(listener)
    }

    pub fn on_error(
        &self,
        listener: impl Fn(Arc<dyn std::error::Error + Send + Sync>) + Send + Sync + 'static,
    ) -> Subscription {
        self.hub.on_error(listener)
    }

    pub async fn open(
        &self,
        params: DesktopSessionOpenParams,
    ) -> Result<DesktopCapabilities, DesktopError> {
        let _guard = self.lifecycle.lock().await;

        let chord = self
            .desired
            .read()
            .await
            .as_ref()
            .and_then(|d| d.chord.clone());
        *self.desired.write().await = Some(DesiredSession {
            params: params.clone(),
            chord,
        });

        let existing_conn = self.connection.read().await.clone();
        if let Some(conn) = existing_conn {
            if conn.rpc.is_alive() {
                return self.open_session(&conn, &params).await;
            }
            self.drop_connection(&conn).await;
        }

        let desired = self.desired.read().await.as_ref().unwrap().clone();
        let (_, caps) = self.start_locked(&desired).await?;
        Ok(caps)
    }

    pub async fn ensure_stop_path(&self, chord: &str) -> Result<StopPathStatus, DesktopError> {
        let _guard = self.lifecycle.lock().await;
        let conn = self.live_locked().await?;

        {
            let current_chord = conn.armed_chord.lock().await;
            if current_chord.as_deref() == Some(chord) {
                return self.call_stop_path(&conn, "stopPath.status", json!({})).await;
            }
        }

        let status = self
            .call_stop_path(&conn, "stopPath.start", json!({ "chord": chord }))
            .await?;
        *conn.armed_chord.lock().await = Some(chord.to_string());
        if let Some(desired) = self.desired.write().await.as_mut() {
            desired.chord = Some(chord.to_string());
        }
        Ok(status)
    }

    pub async fn call(
        &self,
        method: &str,
        params: Value,
        options: CallOptions,
    ) -> Result<Value, DesktopError> {
        let conn = self.live().await?;
        conn.rpc.request(method, params, options).await
    }

    pub async fn capabilities(&self) -> Result<DesktopCapabilities, DesktopError> {
        let opts = CallOptions {
            timeout: Some(CAPABILITIES_TIMEOUT),
            cancel_rx: None,
        };
        let res = self.call("capabilities", json!({}), opts).await?;
        serde_json::from_value::<DesktopCapabilities>(res).map_err(|e| {
            DesktopError::Service(DesktopServiceError::internal(format!(
                "malformed capabilities: {e}"
            )))
        })
    }

    pub async fn stop_path_status(&self) -> Result<StopPathStatus, DesktopError> {
        let conn = self.live().await?;
        self.call_stop_path(&conn, "stopPath.status", json!({})).await
    }

    pub async fn stop(&self) -> Result<StopPathStatus, DesktopError> {
        let conn = self.live().await?;
        self.call_stop_path(&conn, "stopPath.stop", json!({ "source": "host-relay" }))
            .await
    }

    pub async fn resume(&self) -> Result<StopPathStatus, DesktopError> {
        let conn = self.live().await?;
        let token = conn.resume_token.lock().await.clone();
        self.call_stop_path(&conn, "stopPath.resume", json!({ "token": token }))
            .await
    }

    pub async fn close(&self) -> Result<(), DesktopError> {
        let _guard = self.lifecycle.lock().await;
        *self.desired.write().await = None;

        let conn_opt = self.connection.write().await.take();
        if let Some(conn) = conn_opt {
            conn.heartbeat_active.store(false, Ordering::SeqCst);
            let hb_handle = conn.heartbeat_handle.lock().await.take();
            if let Some(h) = hb_handle {
                h.abort();
                let _ = h.await;
            }
            if conn.rpc.is_alive() {
                let _ = tokio::time::timeout(CLOSE_TIMEOUT, async {
                    let _ = conn
                        .rpc
                        .request("session.close", json!({}), CallOptions::default())
                        .await;
                    conn.rpc.end();
                    conn.rpc.exited().await;
                })
                .await;
                conn.rpc.kill();
            }
            conn.rpc.exited().await;
        }
        Ok(())
    }

    async fn live(&self) -> Result<Arc<Connection>, DesktopError> {
        if let Some(conn) = self.connection.read().await.clone()
            && conn.rpc.is_alive() {
                return Ok(conn);
        }
        let _guard = self.lifecycle.lock().await;
        self.live_locked().await
    }

    async fn live_locked(&self) -> Result<Arc<Connection>, DesktopError> {
        let existing_conn = self.connection.read().await.clone();
        if let Some(conn) = existing_conn {
            if conn.rpc.is_alive() {
                return Ok(conn);
            }
            self.drop_connection(&conn).await;
        }

        let desired = self
            .desired
            .read()
            .await
            .clone()
            .ok_or_else(|| DesktopServiceError::closed("desktop session is not open"))?;

        let (conn, _) = self.start_locked(&desired).await?;
        Ok(conn)
    }

    async fn start_locked(
        &self,
        desired: &DesiredSession,
    ) -> Result<(Arc<Connection>, DesktopCapabilities), DesktopError> {
        let child = (self.create_child)().await.map_err(DesktopError::Service)?;
        let hub_clone = self.hub.clone();
        let user_notif = self.on_notification.clone();
        let conn_holder = Arc::clone(&self.connection);

        let hooks = RpcClientHooks {
            on_notification: Arc::new(move |m, p| {
                if let Some(ref cb) = user_notif {
                    cb(&m, &p);
                }
                hub_clone.dispatch(&m, &p);
            }),
            on_exit: Arc::new(move || {
                let holder = Arc::clone(&conn_holder);
                tokio::spawn(async move {
                    let mut guard = holder.write().await;
                    if let Some(conn) = guard.take() {
                        conn.heartbeat_active.store(false, Ordering::SeqCst);
                    }
                });
            }),
        };

        let rpc = RpcClient::new(child, hooks);
        let conn = Arc::new(Connection {
            rpc: Arc::clone(&rpc),
            resume_token: Mutex::new(String::new()),
            armed_chord: Mutex::new(None),
            heartbeat_active: Arc::new(AtomicBool::new(true)),
            heartbeat_handle: Mutex::new(None),
        });

        let caps = match tokio::time::timeout(START_TIMEOUT, self.handshake(&conn, desired)).await {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                rpc.kill();
                return Err(e);
            }
            Err(_) => {
                rpc.kill();
                return Err(DesktopError::Service(DesktopServiceError::timeout(
                    START_TIMEOUT_MESSAGE,
                )));
            }
        };

        *self.connection.write().await = Some(Arc::clone(&conn));

        // Heartbeat task
        let rpc_weak = Arc::downgrade(&rpc);
        let active_hb = Arc::clone(&conn.heartbeat_active);
        let hub_hb = self.hub.clone();
        let handle = tokio::spawn(async move {
            while active_hb.load(Ordering::SeqCst) {
                sleep(HEARTBEAT_INTERVAL).await;
                if !active_hb.load(Ordering::SeqCst) {
                    break;
                }
                let Some(rpc_hb) = rpc_weak.upgrade() else {
                    break;
                };
                if !rpc_hb.is_alive() {
                    break;
                }
                if let Err(e) = rpc_hb
                    .request("stopPath.heartbeat", json!({}), CallOptions::default())
                    .await
                {
                    match e {
                        DesktopError::Service(ref svc_err) => {
                            if svc_err.code() != crate::rpc_client::DesktopServiceErrorCode::Closed
                            {
                                hub_hb.emit_error(Arc::new(e));
                            }
                        }
                        _ => {
                            hub_hb.emit_error(Arc::new(e));
                        }
                    }
                }
            }
        });
        *conn.heartbeat_handle.lock().await = Some(handle);

        Ok((conn, caps))
    }

    async fn handshake(
        &self,
        conn: &Connection,
        desired: &DesiredSession,
    ) -> Result<DesktopCapabilities, DesktopError> {
        let hello_val = conn
            .rpc
            .request("engine.hello", json!({}), CallOptions::default())
            .await?;
        let hello = parse_hello(&hello_val).ok_or_else(|| {
            DesktopServiceError::internal("desktop engine sent a malformed engine.hello result")
        })?;

        if hello.abi != ENGINE_ABI || hello.protocol_version != PROTOCOL_VERSION {
            return Err(DesktopError::AbiMismatch(Box::new(DesktopEngineAbiMismatchError {
                engine_path: conn.rpc.engine_path().to_string(),
                expected_abi: ENGINE_ABI.to_string(),
                expected_protocol: PROTOCOL_VERSION.to_string(),
                actual_abi: hello.abi,
                actual_protocol: hello.protocol_version,
            })));
        }

        let caps = self.open_session(conn, &desired.params).await?;

        if let Some(ref chord) = desired.chord {
            self.call_stop_path(conn, "stopPath.start", json!({ "chord": chord }))
                .await?;
            *conn.armed_chord.lock().await = Some(chord.clone());
        }

        let last_status = self.last_stop_path.lock().unwrap().clone();
        if let Some(ref status) = last_status
            && status.suspended {
                self.call_stop_path(conn, "stopPath.stop", json!({ "source": "host-relay" }))
                    .await?;
        }

        Ok(caps)
    }

    async fn open_session(
        &self,
        conn: &Connection,
        params: &DesktopSessionOpenParams,
    ) -> Result<DesktopCapabilities, DesktopError> {
        let params_val = serde_json::to_value(params)
            .map_err(|e| DesktopServiceError::internal(e.to_string()))?;
        let reply = conn
            .rpc
            .request("session.open", params_val, CallOptions::default())
            .await?;
        let opened = parse_session_opened(&reply).ok_or_else(|| {
            DesktopServiceError::internal("desktop engine sent a malformed session.open result")
        })?;

        *conn.resume_token.lock().await = opened.resume_token;
        Ok(opened.capabilities)
    }

    async fn call_stop_path(
        &self,
        conn: &Connection,
        method: &str,
        params: Value,
    ) -> Result<StopPathStatus, DesktopError> {
        let val = conn
            .rpc
            .request(method, params, CallOptions::default())
            .await?;
        let status = serde_json::from_value::<StopPathStatus>(val).map_err(|_| {
            DesktopServiceError::internal(format!(
                "desktop engine sent a malformed {method} result"
            ))
        })?;
        if let Ok(mut g) = self.last_stop_path.lock() {
            *g = Some(status.clone());
        }
        Ok(status)
    }

    async fn drop_connection(&self, conn: &Connection) {
        conn.heartbeat_active.store(false, Ordering::SeqCst);
        let hb_handle = conn.heartbeat_handle.lock().await.take();
        if let Some(h) = hb_handle {
            h.abort();
            let _ = h.await;
        }
        *self.connection.write().await = None;
    }
}
