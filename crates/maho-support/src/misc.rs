//! Process-wide log hook, PATH lookup and small string helpers.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// A log sink installed by the host.
pub type LogSink = Arc<dyn Fn(&str, Option<&Value>) + Send + Sync>;

static SINK: RwLock<Option<LogSink>> = RwLock::new(None);

/// Installs (or clears) the process-wide log sink.
pub fn set_log_sink(sink: Option<LogSink>) {
    *SINK
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = sink;
}

/// Logs through the installed sink; a no-op until one is set.
pub fn log(message: &str, data: Option<&Value>) {
    let sink = SINK
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(sink) = sink {
        sink(message, data);
    }
}

/// Looks `name` up on `PATH` after rejecting anything that is not a plain program name.
pub fn which_safe(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").or_else(|| std::env::var_os("Path"))?;
    which_safe_in(name, &path.to_string_lossy())
}

/// [`which_safe`] against an explicit search path string.
pub fn which_safe_in(name: &str, search_path: &str) -> Option<PathBuf> {
    if !is_plain_program_name(name) {
        return None;
    }
    let separator = if cfg!(windows) { ';' } else { ':' };
    let has_extension = Path::new(name).extension().is_some();
    for entry in search_path
        .split(separator)
        .filter(|entry| !entry.is_empty())
    {
        let dir = Path::new(entry);
        let mut candidates = vec![dir.join(name)];
        if cfg!(windows) && !has_extension {
            for ext in [".exe", ".cmd", ".bat", ".com"] {
                candidates.push(dir.join(format!("{name}{ext}")));
            }
        }
        if let Some(found) = candidates
            .into_iter()
            .find(|candidate| is_executable_file(candidate))
        {
            return Some(found);
        }
    }
    None
}

fn is_plain_program_name(name: &str) -> bool {
    if name.is_empty() || name == "." || name.contains(['/', '\\', '\0']) || name.contains("..") {
        return false;
    }
    let bytes = name.as_bytes();
    !(bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Concatenation keeping the first occurrence of each string.
pub fn merge_unique(a: &[String], b: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in a.iter().chain(b) {
        if !out.contains(item) {
            out.push(item.clone());
        }
    }
    out
}

/// [`merge_unique`] with case-insensitive comparison (first spelling wins).
pub fn merge_unique_ci(a: &[String], b: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in a.iter().chain(b) {
        if !out
            .iter()
            .any(|seen| seen.to_lowercase() == item.to_lowercase())
        {
            out.push(item.clone());
        }
    }
    out
}

/// `Hh Mm Ss`, `Mm Ss` or `Ss` from milliseconds (negative counts as zero).
pub fn format_duration(ms: i64) -> String {
    let total = ms.max(0) / 1000;
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

/// The first `MAJOR.MINOR.PATCH` (with optional `-pre`/`+build`) in a tool's version output,
/// without a leading `v`. Matches directly preceded by a digit or `:` are ignored.
pub fn extract_semver(output: &str) -> Option<String> {
    if output.trim().is_empty() {
        return None;
    }
    let bytes = output.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        if bytes[start].is_ascii_digit() {
            let preceded_badly = start > 0 && matches!(bytes[start - 1], b'0'..=b'9' | b':');
            if !preceded_badly && let Some(end) = match_semver_at(bytes, start) {
                return Some(output[start..end].to_string());
            }
            // Skip the rest of this digit run.
            while start < bytes.len() && bytes[start].is_ascii_digit() {
                start += 1;
            }
            continue;
        }
        start += 1;
    }
    None
}

fn match_semver_at(bytes: &[u8], start: usize) -> Option<usize> {
    let mut pos = start;
    for part in 0..3 {
        let digits = pos;
        while pos < bytes.len() && bytes[pos].is_ascii_digit() {
            pos += 1;
        }
        if pos == digits {
            return None;
        }
        if part < 2 {
            if bytes.get(pos) != Some(&b'.') {
                return None;
            }
            pos += 1;
        }
    }
    if bytes.get(pos).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    for marker in [b'-', b'+'] {
        if bytes.get(pos) == Some(&marker) {
            let tail = pos + 1;
            let mut end = tail;
            while end < bytes.len()
                && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'.' || bytes[end] == b'-')
            {
                end += 1;
            }
            if end > tail {
                pos = end;
            }
        }
    }
    Some(pos)
}

/// Backslash-escapes the characters that are special inside a double-quoted shell string.
pub fn escape_for_double_quotes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(
            ch,
            '\\' | '$' | '`' | '"' | ';' | '|' | '&' | '#' | '(' | ')'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// The first port at or above `start` that binds on `host` (default `127.0.0.1`).
pub fn find_free_port(start: u16, host: Option<&str>) -> std::io::Result<u16> {
    let host = host.unwrap_or("127.0.0.1");
    for port in start..=u16::MAX {
        if std::net::TcpListener::bind((host, port)).is_ok() {
            return Ok(port);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AddrNotAvailable,
        format!("no free port at or above {start} on {host}"),
    ))
}
