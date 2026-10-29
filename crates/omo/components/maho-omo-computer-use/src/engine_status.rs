use std::fmt;
use std::sync::Arc;
use std::sync::Mutex;

use senpi_desktop_core::error::PermissionDeniedData;
use senpi_desktop_protocol::{DesktopCapabilities, JsonRpcErrorData, StopPathStatus};
use senpi_desktop_service::rpc_client::DesktopError;
use senpi_desktop_service::service::{
    DesktopService, DesktopServiceOptions, DesktopSessionOpenParams,
};

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
        DesktopError::LaunchContext(_) => "launch-context-unavailable".to_string(),
        DesktopError::AbiMismatch(_) => "abi-mismatch".to_string(),
        DesktopError::Service(e) => {
            let msg = e.to_string();
            if msg.contains("launch-context-unavailable")
                || msg.contains("COMPUTER_LAUNCH_CONTEXT_REQUIRED")
            {
                "launch-context-unavailable".to_string()
            } else if msg.contains("native-unavailable") {
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
    }
}

pub fn diagnostic_of(error: &DesktopError) -> Option<String> {
    match error {
        DesktopError::AbiMismatch(_) => Some("abi-mismatch".into()),
        DesktopError::Service(e) => {
            let msg = e.to_string();
            if msg.contains("launch-context-unavailable")
                || msg.contains("COMPUTER_LAUNCH_CONTEXT_REQUIRED")
            {
                Some("launch-context-unavailable".to_string())
            } else if msg.contains("native-unavailable") {
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
    _error_subscription: senpi_desktop_service::notifications::Subscription,
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
                && let Ok(perm_data) =
                    serde_json::from_value::<PermissionDeniedData>(params.clone())
            {
                perm_cb(perm_data);
            }
        });
        svc_opts.on_notification = Some(notif_observer);

        let service = Arc::new(DesktopService::new(svc_opts));

        let state = Arc::new(Mutex::new(EngineState::NotStarted));
        let errors_state = state.clone();
        let error_callback = options.on_error.clone();
        let subscription = service.on_error(move |error| {
            if let Some(error) = error.downcast_ref::<DesktopError>() {
                if let Some(diag) = diagnostic_of(error) {
                    *errors_state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        EngineState::Diagnostic(diag);
                }
                if let Some(callback) = &error_callback {
                    callback(engine_error_code(error));
                }
            } else if let Some(callback) = &error_callback {
                callback("other".into());
            }
        });
        Self {
            service,
            state,
            _error_subscription: subscription,
            on_error: options.on_error,
            on_permission_required: options.on_permission_required,
        }
    }

    pub fn service(&self) -> Arc<DesktopService> {
        Arc::clone(&self.service)
    }

    pub async fn engine_state(&self) -> EngineState {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub async fn set_engine_state(&self, new_state: EngineState) {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = new_state;
    }

    pub async fn open(
        &self,
        params: DesktopSessionOpenParams,
    ) -> Result<DesktopCapabilities, DesktopError> {
        match self.service.open(params).await {
            Ok(caps) => {
                *self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = EngineState::Ready;
                Ok(caps)
            }
            Err(e) => {
                let code = engine_error_code(&e);
                self.record_error(&e, &code);
                if let Some(diag) = diagnostic_of(&e) {
                    *self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        EngineState::Diagnostic(diag);
                }
                if let Some(diag) = diagnostic_of(&e) {
                    *self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        EngineState::Diagnostic(diag);
                }
                Err(e)
            }
        }
    }

    pub async fn ensure_stop_path(&self, hotkey: &str) -> Result<StopPathStatus, DesktopError> {
        match self.service.ensure_stop_path(hotkey).await {
            Ok(s) => {
                self.observe_success().await;
                Ok(s)
            }
            Err(e) => {
                let code = engine_error_code(&e);
                self.record_error(&e, &code);
                if let Some(diag) = diagnostic_of(&e) {
                    *self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        EngineState::Diagnostic(diag);
                }
                Err(e)
            }
        }
    }

    pub async fn capabilities(&self) -> Result<DesktopCapabilities, DesktopError> {
        match self.service.capabilities().await {
            Ok(c) => {
                self.observe_success().await;
                Ok(c)
            }
            Err(e) => {
                let code = engine_error_code(&e);
                self.record_error(&e, &code);
                if let Some(diag) = diagnostic_of(&e) {
                    *self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        EngineState::Diagnostic(diag);
                }
                Err(e)
            }
        }
    }

    pub async fn stop_path_status(&self) -> Result<StopPathStatus, DesktopError> {
        match self.service.stop_path_status().await {
            Ok(s) => {
                self.observe_success().await;
                Ok(s)
            }
            Err(e) => {
                let code = engine_error_code(&e);
                self.record_error(&e, &code);
                if let Some(diag) = diagnostic_of(&e) {
                    *self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        EngineState::Diagnostic(diag);
                }
                Err(e)
            }
        }
    }

    pub async fn stop(&self) -> Result<StopPathStatus, DesktopError> {
        match self.service.stop().await {
            Ok(s) => {
                self.observe_success().await;
                Ok(s)
            }
            Err(e) => {
                let code = engine_error_code(&e);
                self.record_error(&e, &code);
                if let Some(diag) = diagnostic_of(&e) {
                    *self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        EngineState::Diagnostic(diag);
                }
                Err(e)
            }
        }
    }

    pub async fn resume(&self) -> Result<StopPathStatus, DesktopError> {
        match self.service.resume().await {
            Ok(s) => {
                self.observe_success().await;
                Ok(s)
            }
            Err(e) => {
                let code = engine_error_code(&e);
                self.record_error(&e, &code);
                if let Some(diag) = diagnostic_of(&e) {
                    *self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        EngineState::Diagnostic(diag);
                }
                Err(e)
            }
        }
    }

    async fn observe_success(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(&*state, EngineState::NotStarted)
            || matches!(&*state, EngineState::Diagnostic(code) if code == "launch-context-unavailable")
        {
            *state = EngineState::Ready;
        }
    }
    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
        options: senpi_desktop_service::rpc_client::CallOptions,
    ) -> Result<serde_json::Value, DesktopError> {
        match self.service.call(method, params, options).await {
            Ok(value) => {
                self.observe_success().await;
                Ok(value)
            }
            Err(error) => {
                self.record_error(&error, &engine_error_code(&error));
                if let Some(diag) = diagnostic_of(&error) {
                    *self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        EngineState::Diagnostic(diag);
                }
                Err(error)
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
            && let Some(ref on_perm) = self.on_permission_required
        {
            on_perm(perm_denied.clone());
        }
    }
}

#[cfg(test)]
mod observation_tests {
    use super::*;
    #[tokio::test]
    async fn successful_observations_recover_launch_but_not_binary_diagnostics() {
        let tracked = TrackedDesktopService::new(TrackedDesktopServiceOptions {
            service_options: DesktopServiceOptions::default(),
            on_error: None,
            on_permission_required: None,
        });
        tracked.observe_success().await;
        assert_eq!(tracked.engine_state().await, EngineState::Ready);
        for diagnostic in [
            "launch-context-unavailable",
            "abi-mismatch",
            "quarantined",
            "native-unavailable",
        ] {
            tracked
                .set_engine_state(EngineState::Diagnostic(diagnostic.into()))
                .await;
            tracked.observe_success().await;
            assert_eq!(
                tracked.engine_state().await,
                if diagnostic == "launch-context-unavailable" {
                    EngineState::Ready
                } else {
                    EngineState::Diagnostic(diagnostic.into())
                }
            );
        }
    }
}
