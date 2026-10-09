//! Computer activation and handle lifecycle matching upstream `src/activation.ts`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

use senpi_desktop_protocol::{ComputerSessionSnapshot, DesktopCapabilities, StopPathStatus};
use senpi_desktop_service::rpc_client::DesktopError;
use senpi_desktop_service::service::DesktopService;

use crate::session::{run_snapshot, session_open_params, ComputerHostContext};
use crate::settings::ComputerSettings;

pub type ActivationListener = Arc<dyn Fn(bool) + Send + Sync>;

/// `/computer off` switched computer use off for this session.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Computer use is off for this session; the user can turn it on with /computer on.")]
pub struct ComputerDisabledError;

struct OpenedSession {
    key: String,
}

/// The per-agent-session computer state shared by the tool, `/computer`, and the host extension.
pub struct ComputerHandle {
    service: Arc<DesktopService>,
    settings_fn: Arc<dyn Fn() -> ComputerSettings + Send + Sync>,
    listeners: Arc<Mutex<Vec<ActivationListener>>>,
    enabled_override: Arc<RwLock<Option<bool>>>,
    opened: Arc<Mutex<Option<OpenedSession>>>,
    active: Arc<AtomicBool>,
}

impl ComputerHandle {
    pub fn new(
        service: Arc<DesktopService>,
        settings: impl Fn() -> ComputerSettings + Send + Sync + 'static,
    ) -> Self {
        Self {
            service,
            settings_fn: Arc::new(settings),
            listeners: Arc::new(Mutex::new(Vec::new())),
            enabled_override: Arc::new(RwLock::new(None)),
            opened: Arc::new(Mutex::new(None)),
            active: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn service(&self) -> Arc<DesktopService> {
        self.service.clone()
    }

    pub fn settings(&self) -> ComputerSettings {
        (self.settings_fn)()
    }

    pub async fn enabled(&self) -> bool {
        let override_val = *self.enabled_override.read().await;
        override_val.unwrap_or_else(|| self.settings().enabled)
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    pub async fn is_running(&self) -> bool {
        self.opened.lock().await.is_some()
    }

    pub async fn set_enabled(&self, enabled: bool) {
        *self.enabled_override.write().await = Some(enabled);
    }

    pub async fn on_activation_change(&self, listener: ActivationListener) {
        self.listeners.lock().await.push(listener);
    }

    pub async fn activate(&self, context: &ComputerHostContext) -> Result<StopPathStatus, DesktopError> {
        if !self.enabled().await {
            return Err(senpi_desktop_service::rpc_client::DesktopServiceError::closed(
                "Computer use is off for this session; the user can turn it on with /computer on.",
            ).into());
        }
        let settings = self.settings();
        self.ensure_open(&settings, context).await?;
        let status = self.service.ensure_stop_path(&settings.stop_hotkey).await?;
        self.set_active_state(true).await;
        Ok(status)
    }

    pub async fn start(&self, context: &ComputerHostContext) -> Result<StopPathStatus, DesktopError> {
        self.activate(context).await
    }

    pub fn snapshot(&self, context: &ComputerHostContext, read_only: bool) -> ComputerSessionSnapshot {
        run_snapshot(&self.settings(), context, read_only)
    }

    pub async fn deactivate(&self) -> Result<(), DesktopError> {
        self.close().await?;
        self.set_active_state(false).await;
        Ok(())
    }

    pub async fn close(&self) -> Result<(), DesktopError> {
        *self.opened.lock().await = None;
        self.service.close().await?;
        Ok(())
    }

    pub async fn dispose(&self) -> Result<(), DesktopError> {
        self.close().await
    }

    pub async fn stop_path_status(&self) -> Result<Option<StopPathStatus>, DesktopError> {
        if self.is_running().await {
            Ok(Some(self.service.stop_path_status().await?))
        } else {
            Ok(None)
        }
    }

    pub async fn stop(&self) -> Result<Option<StopPathStatus>, DesktopError> {
        if self.is_running().await {
            Ok(Some(self.service.stop().await?))
        } else {
            Ok(None)
        }
    }

    pub async fn resume(&self) -> Result<Option<StopPathStatus>, DesktopError> {
        if self.is_running().await {
            Ok(Some(self.service.resume().await?))
        } else {
            Ok(None)
        }
    }

    pub async fn capabilities(&self) -> Result<Option<DesktopCapabilities>, DesktopError> {
        if self.is_running().await {
            Ok(Some(self.service.capabilities().await?))
        } else {
            Ok(None)
        }
    }

    async fn ensure_open(
        &self,
        settings: &ComputerSettings,
        context: &ComputerHostContext,
    ) -> Result<(), DesktopError> {
        let params = session_open_params(settings, context);
        let key = serde_json::to_string(&params).unwrap_or_default();

        let mut opened_guard = self.opened.lock().await;
        if let Some(opened) = opened_guard.as_ref()
            && opened.key == key {
            return Ok(());
        }

        match self.service.open(params).await {
            Ok(_) => {
                *opened_guard = Some(OpenedSession { key });
                Ok(())
            }
            Err(e) => {
                *opened_guard = None;
                Err(e)
            }
        }
    }

    async fn set_active_state(&self, active: bool) {
        let old = self.active.swap(active, Ordering::SeqCst);
        if old != active {
            let listeners = self.listeners.lock().await.clone();
            for listener in listeners {
                listener(active);
            }
        }
    }
}
