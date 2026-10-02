use std::{io::{self, Write}, path::Path, sync::Arc};
use maho_core::{config::get_debug_log_path, output_guard::{restore_stderr, take_over_stderr}, sensitive_output::redact_sensitive_output};

pub fn append_hidden_interactive_stderr(path: &Path, text: &str, timestamp: &str) -> io::Result<()> {
    if text.is_empty() { return Ok(()); }
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    let mut options = std::fs::OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let redacted = redact_sensitive_output(text);
    write!(file, "[{timestamp}] hidden stderr while TUI active\n{redacted}{}", if redacted.ends_with('\n') { "" } else { "\n" })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn take_over_interactive_stderr() {
    take_over_stderr(Some(Arc::new(|text| {
        let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        if let Err(error) = append_hidden_interactive_stderr(Path::new(&get_debug_log_path()), text, &timestamp) {
            // The existing output guard catches a failed diagnostic sink and emits its redacted fallback.
            std::panic::resume_unwind(Box::new(error));
        }
    })), Some(Arc::new(redact_sensitive_output)));
}

pub fn restore_interactive_stderr() { restore_stderr(); }
