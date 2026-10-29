//! Computer session context and options matching upstream `src/session.ts`.

use std::path::PathBuf;
use std::sync::Arc;
use maho_tools::ToolContext;
use senpi_desktop_protocol::ComputerSessionSnapshot;
use senpi_desktop_service::service::{
    CaptureCapsParams, DesktopSessionOpenParams, ScreenshotGcParams,
};
use serde::{Deserialize, Serialize};

use crate::settings::ComputerSettings;

pub const AUDIT_FILE_NAME: &str = ".computer-audit.jsonl";

/// The fields of the host's active model (`@earendil-works/pi-ai` `Model`) the capture policy reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputerModel {
    pub id: String,
    pub provider: String,
    pub api: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<serde_json::Value>,
}

pub trait ComputerSessionManager: Send + Sync {
    fn get_session_id(&self) -> String;
    fn get_session_dir(&self) -> PathBuf;
}

#[derive(Clone)]
pub struct SimpleSessionManager {
    pub session_id: String,
    pub session_dir: PathBuf,
}

impl ComputerSessionManager for SimpleSessionManager {
    fn get_session_id(&self) -> String {
        self.session_id.clone()
    }
    fn get_session_dir(&self) -> PathBuf {
        self.session_dir.clone()
    }
}

/// What the host knows about the agent session; a structural subset of coding-agent's `ExtensionContext`.
pub struct ComputerHostContext {
    pub cwd: PathBuf,
    pub model: Option<ComputerModel>,
    pub session_manager: Arc<dyn ComputerSessionManager>,
}

impl ComputerHostContext {
    pub fn new(
        cwd: impl Into<PathBuf>,
        model: Option<ComputerModel>,
        session_id: impl Into<String>,
        session_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            cwd: cwd.into(),
            model,
            session_manager: Arc::new(SimpleSessionManager {
                session_id: session_id.into(),
                session_dir: session_dir.into(),
            }),
        }
    }

    pub fn from_tool_context(ctx: &dyn ToolContext) -> Self {
        let cwd = ctx.cwd().to_path_buf();
        let model = ctx.model().map(|m| ComputerModel {
            id: m.id.clone(),
            provider: m.provider.clone(),
            api: m.api.clone(),
            compat: None,
        });
        let sm = ctx.session_manager();
        let s_id = sm.session_id().to_string();
        let s_dir = sm
            .session_file()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| cwd.clone());
        Self::new(cwd, model, s_id, s_dir)
    }
}

/// With screenshot GC off nothing may ever count as stale; the engine has no separate off switch.
pub const GC_NEVER_STALE_MS: u64 = 9_007_199_254_740_991;

/// Whether the model clicks in the pixels it was shown after a transport resize it cannot see.
pub fn uses_coordinate_safe_image_sizing(model: Option<&ComputerModel>) -> bool {
    let model = match model {
        Some(m) => m,
        None => return false,
    };
    if let Some(compat) = &model.compat
        && compat
            .get("supportsImageDetailOriginal")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
    {
        return true;
    }
    model.api == "anthropic-messages"
        || model.provider == "anthropic"
        || model.id.to_lowercase().contains("claude")
}

/// `session.open` params from the settings, the active model, and the session directory.
pub fn session_open_params(
    settings: &ComputerSettings,
    context: &ComputerHostContext,
) -> DesktopSessionOpenParams {
    let audit_path = if settings.audit_log.enabled {
        Some(
            context
                .session_manager
                .get_session_dir()
                .join(AUDIT_FILE_NAME)
                .to_string_lossy()
                .to_string(),
        )
    } else {
        None
    };

    DesktopSessionOpenParams {
        display: Some(settings.display.clone()),
        macos_canary: Some(settings.macos_canary.clone()),
        allow_host_relay_only_stop: Some(settings.allow_host_relay_only_stop),
        audit_path,
        artifact_dir: Some(std::env::temp_dir().to_string_lossy().to_string()),
        screenshot_gc: Some(ScreenshotGcParams {
            stale_ms: Some(if settings.screenshot_gc.enabled {
                settings.screenshot_gc.stale_ms
            } else {
                GC_NEVER_STALE_MS
            }),
            scan_interval_ms: Some(settings.screenshot_gc.scan_interval_ms),
        }),
        capture_caps: Some(CaptureCapsParams {
            max_width: Some(settings.max_width),
            max_height: Some(settings.max_height),
            coordinate_safe: Some(uses_coordinate_safe_image_sizing(context.model.as_ref())),
            max_bytes: Some(settings.screenshot_max_bytes),
        }),
    }
}

/// The frozen per-run settings `run_computer_code` hands to the facade.
pub fn run_snapshot(
    settings: &ComputerSettings,
    context: &ComputerHostContext,
    read_only: bool,
) -> ComputerSessionSnapshot {
    ComputerSessionSnapshot {
        cwd: context.cwd.to_string_lossy().to_string(),
        session_id: context.session_manager.get_session_id(),
        capture_max_width: settings.max_width,
        capture_max_height: settings.max_height,
        capture_max_bytes: settings.screenshot_max_bytes,
        display: settings.display.clone(),
        read_only,
        stop_hotkey: settings.stop_hotkey.clone(),
        allow_host_relay_only_stop: settings.allow_host_relay_only_stop,
    }
}
