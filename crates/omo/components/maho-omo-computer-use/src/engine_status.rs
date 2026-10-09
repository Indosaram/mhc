use std::fmt;
use std::sync::Arc;
use tokio::sync::Mutex;

use senpi_desktop_core::error::PermissionDeniedData;
use senpi_desktop_protocol::{DesktopCapabilities, JsonRpcErrorData, StopPathStatus};
use senpi_desktop_service::rpc_client::DesktopError;
use senpi_desktop_service::service::{DesktopService, DesktopServiceOptions, DesktopSessionOpenParams};

pub type EngineDiagnostic = String;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineState {
    NotStarted,
    Ready,
    Diagnostic(String),
}

impl fmt::Display for EngineState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotStarted => write!(f, "not started"),
            Self::Ready => write!(f, "ready"),
            Self::Diagnostic(msg) => write!(f, "{msg}"),
        }
    }
}

pub fn engine_error_code(error: &DesktopError) -> String {
    match error {
        DesktopError::Service(e) => {
            let msg = e.to_string();
            if msg.contains("native-unavailable") {
                "native-unavailable".to_string()
            } else if msg.contains("quarantined") {
                "quarantined".to_string()
            } else if msg.contains("abi-mismatch") {
                "abi-mismatch".to_string()
            } else {
                format!("{:?}", e.code)
            }
        }
        DesktopError::Rpc(rpc_err) => {
            if let Some(ref data) = rpc_err.data {
                match data {
                    JsonRpcErrorData::Engine(engine_data) => {
                        format!("{:?}", engine_data.code)
                    }
                    _ => "other".to_string(),
                }
            } else {
                "other".to_string()
            }
        }
        _ => "other".to_string(),
    }
}

pub fn diagnostic_of(error: &DesktopError) -> Option<String> {
    match error {
        DesktopError::Service(e) => {
            let msg = e.to_string();
            if msg.contains("native-unavailable") {
                Some("native-unavailable".to_string())
            } else if msg.contains("quarantined") {
                Some("quarantined".to_string())
            } else if msg.contains("abi-mismatch") {
                Some("abi-mismatch".to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}

pub struct TrackedDesktopServiceOptions {
    pub service_options: DesktopServiceOptions,
    pub on_error: Option<Arc<dyn Fn(String) + Send + Sync>>,
    pub on_permission_required: Option<Arc<dyn Fn(PermissionDeniedData) + Send + Sync>>,
}

pub struct TrackedDesktopService {
    service: Arc<DesktopService>,
    state: Arc<Mutex<EngineState>>,
    on_error: Option<Arc<dyn Fn(String) + Send + Sync>>,
    on_permission_required: Option<Arc<dyn Fn(PermissionDeniedData) + Send + Sync>>,
}

impl TrackedDesktopService {
    pub fn new(options: TrackedDesktopServiceOptions) -> Self {
        let on_err = options.on_error.clone();
        let on_perm = options.on_permission_required.clone();

        let mut svc_opts = options.service_options;
        let prev_notif = svc_opts.on_notification.take();

        let notif_observer = Arc::new(move |method: &str, params: &serde_json::Value| {
            if let Some(ref prev) = prev_notif {
                prev(method, params);
            }
            if method == "error" || method == "engine_error" {
                let code = if let Some(err_str) = params.as_str() {
                    err_str.to_string()
                } else if let Some(code_val) = params.get("code").and_then(|c| c.as_str()) {
                    code_val.to_string()
                } else {
                    params.to_string()
                };
                if let Some(ref cb) = on_err {
                    cb(code);
                }
            } else if (method == "permission_required" || method == "permission_denied")
                && let Some(ref perm_cb) = on_perm
                && let Ok(perm_data) = serde_json::from_value::<PermissionDeniedData>(params.clone()) {
                perm_cb(perm_data);
            }
        });
        svc_opts.on_notification = Some(notif_observer);

        let service = Arc::new(DesktopService::new(svc_opts));

        Self {
            service,
            state: Arc::new(Mutex::new(EngineState::NotStarted)),
            on_error: options.on_error,
            on_permission_required: options.on_permission_required,
        }
    }

    pub fn service(&self) -> Arc<DesktopService> {
        Arc::clone(&self.service)
    }

    pub async fn engine_state(&self) -> EngineState {
        self.state.lock().await.clone()
    }

    pub async fn set_engine_state(&self, new_state: EngineState) {
        *self.state.lock().await = new_state;
    }

    pub async fn open(&self, params: DesktopSessionOpenParams) -> Result<DesktopCapabilities, DesktopError> {
        match self.service.open(params).await {
            Ok(caps) => {
                *self.state.lock().await = EngineState::Ready;
                Ok(caps)
            }
            Err(e) => {
                let code = engine_error_code(&e);
                self.record_error(&e, &code);
                if let Some(diag) = diagnostic_of(&e) {
                    *self.state.lock().await = EngineState::Diagnostic(diag);
                }
                Err(e)
            }
        }
    }

    pub async fn ensure_stop_path(&self, hotkey: &str) -> Result<StopPathStatus, DesktopError> {
        match self.service.ensure_stop_path(hotkey).await {
            Ok(s) => Ok(s),
            Err(e) => {
                let code = engine_error_code(&e);
                self.record_error(&e, &code);
                Err(e)
            }
        }
    }

    pub async fn capabilities(&self) -> Result<DesktopCapabilities, DesktopError> {
        match self.service.capabilities().await {
            Ok(c) => Ok(c),
            Err(e) => {
                let code = engine_error_code(&e);
                self.record_error(&e, &code);
                Err(e)
            }
        }
    }

    pub async fn stop_path_status(&self) -> Result<StopPathStatus, DesktopError> {
        match self.service.stop_path_status().await {
            Ok(s) => Ok(s),
            Err(e) => {
                let code = engine_error_code(&e);
                self.record_error(&e, &code);
                Err(e)
            }
        }
    }

    pub async fn stop(&self) -> Result<StopPathStatus, DesktopError> {
        match self.service.stop().await {
            Ok(s) => Ok(s),
            Err(e) => {
                let code = engine_error_code(&e);
                self.record_error(&e, &code);
                Err(e)
            }
        }
    }

    pub async fn resume(&self) -> Result<StopPathStatus, DesktopError> {
        match self.service.resume().await {
            Ok(s) => Ok(s),
            Err(e) => {
                let code = engine_error_code(&e);
                self.record_error(&e, &code);
                Err(e)
            }
        }
    }

    pub async fn close(&self) -> Result<(), DesktopError> {
        self.service.close().await
    }

    fn record_error(&self, error: &DesktopError, code: &str) {
        if let Some(ref on_err) = self.on_error {
            on_err(code.to_string());
        }

        if let DesktopError::Rpc(rpc_err) = error
            && let Some(JsonRpcErrorData::Engine(engine_data)) = &rpc_err.data
            && let Some(ref perm_denied) = engine_data.permission
            && let Some(ref on_perm) = self.on_permission_required {
            on_perm(perm_denied.clone());
        }
    }
}
