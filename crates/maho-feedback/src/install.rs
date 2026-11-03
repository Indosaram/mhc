//! The reinstall step of messages that report a missing part of the installation (delta D4).
//! This crate is linked into the binary and has no loader-failure message of its own; the step is
//! shared with any product message about a missing or unreadable installed part.

/// Reinstall command set by the launcher/installer.
pub const REINSTALL_COMMAND_ENV: &str = "MAHO_REINSTALL_COMMAND";
/// Legacy name of [`REINSTALL_COMMAND_ENV`], read when that one is unset or blank.
pub const LEGACY_REINSTALL_COMMAND_ENV: &str = "OMO_REINSTALL_COMMAND";

/// The reinstall step: the exact command when one is configured (trimmed, non-blank), else a
/// plain instruction without a command.
pub fn reinstall_step(env: &dyn Fn(&str) -> Option<String>) -> String {
    let command = [REINSTALL_COMMAND_ENV, LEGACY_REINSTALL_COMMAND_ENV]
        .into_iter()
        .filter_map(env)
        .map(|value| value.trim().to_owned())
        .find(|value| !value.is_empty());
    match command {
        Some(command) => format!("Reinstall mhc by running: {command}"),
        None => "Reinstall mhc.".to_owned(),
    }
}

/// A missing-installation message: what failed, that part of the installation is missing, the
/// reinstall step, and when a running session is affected, that a new session is needed.
pub fn missing_installation_message(
    what_failed: &str,
    session_affected: bool,
    env: &dyn Fn(&str) -> Option<String>,
) -> String {
    let mut message = format!(
        "{what_failed}: part of the mhc installation is missing. {}",
        reinstall_step(env)
    );
    if session_affected {
        message.push_str(" Then start a new session.");
    }
    message
}
