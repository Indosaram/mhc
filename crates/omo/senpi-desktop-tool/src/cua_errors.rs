//! CUA error recovery and translation matching upstream `src/cua-errors.ts`.

use senpi_desktop_protocol::PermissionDeniedData;
use senpi_desktop_service::rpc_client::DesktopLaunchContextError;
use serde::{Deserialize, Serialize};

fn engine_to_computer_code(code: &str) -> &'static str {
    match code {
        "Suspended" => "COMPUTER_SUSPENDED",
        "StopPathUnavailable" => "COMPUTER_SUPERVISOR_NOT_LIVE",
        "PermissionDenied" => "COMPUTER_PERMISSION_REQUIRED",
        "InvalidCoordinateFrame" => "COMPUTER_DISPLAY_STALE",
        "ScreenLocked" => "COMPUTER_SCREEN_LOCKED",
        "CaptureFailed" => "COMPUTER_SCREENSHOT_FAILED",
        "CursorRestoreFailed" => "COMPUTER_CURSOR_RESTORE_FAILED",
        "FocusRestoreFailed" => "COMPUTER_FOCUS_RESTORE_FAILED",
        "BackgroundUnavailable" => "COMPUTER_BACKGROUND_UNAVAILABLE",
        "Cancelled" | "Timeout" | "timeout" | "aborted" => "COMPUTER_CANCELLED",
        other if other.starts_with("COMPUTER_") => "COMPUTER_CUSTOM",
        _ => "COMPUTER_ERROR",
    }
}

fn recovery_hint(code: &str, stop_hotkey: &str) -> Option<String> {
    let wait_for_user = format!(
        "Stop and wait for the user (stop chord: {stop_hotkey}); only the user resumes, with /computer resume."
    );
    match code {
        "COMPUTER_COORD_INVALID" => {
            Some("Capture a fresh screenshot and use coordinates within its frame.".to_string())
        }
        "COMPUTER_DISPLAY_STALE" => Some(
            "Capture a fresh screenshot before acting; the display changed since the last screenshot."
                .to_string(),
        ),
        "COMPUTER_SUSPENDED" | "COMPUTER_SUPERVISOR_NOT_LIVE" | "COMPUTER_CANCELLED" => {
            Some(wait_for_user)
        }
        "COMPUTER_PERMISSION_REQUIRED" => Some(
            "Do not retry until the user says the grant is done and the app was relaunched."
                .to_string(),
        ),
        "COMPUTER_PERMISSION_DENIED" => {
            Some("A permission rule denied this action; do not retry it.".to_string())
        }
        "COMPUTER_CURSOR_RESTORE_FAILED" | "COMPUTER_FOCUS_RESTORE_FAILED" => Some(
            "Input may have completed, but the previous state was not restored. Ask the user to inspect the desktop before retrying."
                .to_string(),
        ),
        "COMPUTER_BACKGROUND_UNAVAILABLE" => Some(
            "This window cannot take background input; use the computer tool's accessibility actions instead."
                .to_string(),
        ),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerFailure {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relaunch_required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

pub fn launch_context_failure(error: &DesktopLaunchContextError) -> ComputerFailure {
    ComputerFailure {
        code: "COMPUTER_LAUNCH_CONTEXT_REQUIRED".to_string(),
        message: error.message.clone(),
        permission: None,
        settings_url: None,
        app: None,
        relaunch_required: None,
        reason: Some(error.reason.clone()),
    }
}

pub fn computer_failure(
    code: &str,
    reason: &str,
    stop_hotkey: &str,
    permission: Option<&PermissionDeniedData>,
) -> ComputerFailure {
    let mapped = if code.starts_with("COMPUTER_") {
        code.to_string()
    } else {
        engine_to_computer_code(code).to_string()
    };
    let recovery = recovery_hint(&mapped, stop_hotkey);
    let message = match recovery {
        Some(hint) => format!("{mapped}: {reason} {hint}"),
        None => format!("{mapped}: {reason}"),
    };

    ComputerFailure {
        code: mapped,
        message,
        permission: permission.map(|p| match p.permission { senpi_desktop_protocol::TccPermission::ScreenRecording => "screen_recording", senpi_desktop_protocol::TccPermission::Accessibility => "accessibility" }.to_string()),
        settings_url: permission.map(|p| p.settings_url.clone()),
        app: permission.map(|p| p.app.clone()),
        relaunch_required: permission.map(|p| p.relaunch_required),
        reason: None,
    }
}
