//! Port of senpi packages/coding-agent/src/core/hidden-stdout-log.ts.

use std::io::Write;
use std::path::Path;

use chrono::{SecondsFormat, Utc};

use crate::config::get_debug_log_path;
use crate::sensitive_output::redact_sensitive_output;

fn append_debug_log_entry(header: &str, text: &str) {
    let debug_log_path = get_debug_log_path();
    let prefix = format!("[{}] {header}\n", Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true));
    let redacted = redact_sensitive_output(text);
    let suffix = if redacted.ends_with('\n') { "" } else { "\n" };
    if let Some(parent) = Path::new(&debug_log_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&debug_log_path) {
        let _ = file.write_all(format!("{prefix}{redacted}{suffix}").as_bytes());
    }
    set_private_mode(&debug_log_path);
}

#[cfg(unix)]
fn set_private_mode(path: &str) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn set_private_mode(_path: &str) {}

pub fn append_hidden_tui_stdout(text: &str) {
    if text.is_empty() {
        return;
    }
    append_debug_log_entry("hidden stdout while TUI active", text);
}

/// Records a fatal uncaught crash in the brand debug log. Callers invoke this BEFORE the terminal
/// handoff and swallow any failure: writing telemetry may never alter the crash path.
pub fn append_uncaught_crash_log(origin: &str, error: &str) {
    append_debug_log_entry(&format!("uncaught crash ({origin})"), &describe_crash(error));
}

/// senpi accepts an unknown; the Rust port takes the already-stringified crash text.
pub fn describe_crash(error: &str) -> String {
    error.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_hidden_stdout_write_is_a_noop() {
        append_hidden_tui_stdout("");
    }

    #[test]
    fn describe_crash_preserves_the_text() {
        assert_eq!(describe_crash("Error: boom"), "Error: boom");
    }
}
