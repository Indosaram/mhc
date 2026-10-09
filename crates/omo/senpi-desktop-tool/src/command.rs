//! /computer slash command router matching upstream `src/command.ts`.

use senpi_desktop_protocol::{DesktopCapabilities, StopPathStatus};

use crate::activation::ComputerHandle;
use crate::session::ComputerHostContext;

pub const COMPUTER_SUBCOMMANDS: &[&str] = &["on", "off", "status", "stop", "resume"];
pub const COMPUTER_COMMAND_USAGE: &str = "Usage: /computer on|off|status|stop|resume";

pub fn describe_stop_path(status: &StopPathStatus) -> String {
    let reason_part = match &status.reason {
        Some(r) if !r.is_empty() => format!(" reason={r}"),
        _ => String::new(),
    };
    format!(
        "stopPath={} suspended={}{}",
        match status.stop_path { senpi_desktop_protocol::StopPathKind::Global => "global", senpi_desktop_protocol::StopPathKind::HostRelay => "host-relay", senpi_desktop_protocol::StopPathKind::None => "none" }, status.suspended, reason_part
    )
}

pub fn describe_capabilities(caps: Option<&DesktopCapabilities>) -> String {
    match caps {
        None => "stopPath=n/a focusGuard=n/a (engine not started)".to_string(),
        Some(c) => {
            let stop_reason = match &c.stop_reason {
                Some(r) if !r.is_empty() => format!(" stopReason={r}"),
                _ => String::new(),
            };
            format!(
                "backend={} stopPath={}{} focusGuard={} capturePermission={} inputPermission={} axPermission={} backgroundWindowInput={} screenLocked={}",
                c.backend,
                c.stop_path,
                stop_reason,
                c.focus_guard,
                c.capture_permission,
                c.input_permission,
                c.ax_permission,
                c.background_window_input,
                c.screen_locked
            )
        }
    }
}

pub async fn status(handle: &ComputerHandle) -> String {
    let stop_path = handle.stop_path_status().await.ok().flatten();
    let caps = handle.capabilities().await.ok().flatten();
    let enabled = handle.enabled().await;
    let active = handle.is_active();
    let running = handle.is_running().await;

    let stop_str = match &stop_path {
        Some(sp) => describe_stop_path(sp),
        None => "n/a (engine not started)".to_string(),
    };

    [
        format!("Computer use: enabled={enabled} active={active} engine={}", if running { "running" } else { "not started" }),
        format!("capabilities: {}", describe_capabilities(caps.as_ref())),
        format!("stop: {stop_str}"),
        "permissions: inspection needs computer:read, input and mutation need computer:exec; non-interactive modes block `ask` unless a rule pre-allows the tier".to_string(),
    ].join("
")
}

pub async fn run_computer_command(
    args: &str,
    handle: &ComputerHandle,
    context: &ComputerHostContext,
) -> String {
    let word = args.trim().to_lowercase();
    let subcommand = if word.is_empty() { "status" } else { word.as_str() };

    match subcommand {
        "on" => {
            handle.set_enabled(true).await;
            match handle.activate(context).await {
                Ok(armed) => format!("Computer use on for this session: {}", describe_stop_path(&armed)),
                Err(e) => format!("Failed to turn computer use on: {e}"),
            }
        }
        "off" => {
            handle.set_enabled(false).await;
            let _ = handle.deactivate().await;
            "Computer use off for this session.".to_string()
        }
        "status" => status(handle).await,
        "stop" => {
            match handle.stop().await {
                Ok(Some(stopped)) => format!("Computer input stopped: {}", describe_stop_path(&stopped)),
                _ => "Computer use is not running; nothing to stop.".to_string(),
            }
        }
        "resume" => {
            match handle.resume().await {
                Ok(Some(resumed)) => format!("Computer input resumed: {}", describe_stop_path(&resumed)),
                _ => "Computer use is not running; nothing to resume.".to_string(),
            }
        }
        _ => COMPUTER_COMMAND_USAGE.to_string(),
    }
}
