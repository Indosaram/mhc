//! Port of senpi packages/agent/src/harness/env/nodejs.ts.
//!
//! senpi uses `node:fs/promises` and `node:child_process`; the crate's dependency set (owned by
//! todo 14) has no `tokio` `fs`/`process` features, so the equivalent `std::fs` and
//! `std::process` calls are used. The observable contract — results, error codes, messages,
//! timeout/abort handling, bounded capture and spilling — matches the TS implementation.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use maho_ai::types::BoxFuture;
use maho_ai::utils::abort::AbortSignal;

use super::super::context::Context;
use super::super::result::{Result, err, ok};
use super::super::types::{
    ExecutionError, ExecutionErrorCode, FileError, FileErrorCode, FileInfo, FileKind, FileSystem, Shell,
    ShellExecOptions, ShellExecResult, TextLine, TextLineReader,
};
use super::super::utils::output_capture::{OutputCapture, OutputCaptureHandlers};

const MAX_TIMEOUT_MS: u64 = 2_147_483_647;
const MAX_TIMEOUT_SECONDS: f64 = MAX_TIMEOUT_MS as f64 / 1000.0;
const EXIT_STDIO_GRACE_MS: u64 = 100;

/// `resolveTimeoutMs(timeout)`.
fn resolve_timeout_ms(timeout: Option<f64>) -> Result<Option<u64>, ExecutionError> {
    let Some(timeout) = timeout else {
        return ok(None);
    };
    if !timeout.is_finite() || timeout <= 0.0 {
        return err(ExecutionError::new(
            ExecutionErrorCode::Timeout,
            "Invalid timeout: must be a finite number of seconds",
        ));
    }
    let timeout_ms = timeout * 1000.0;
    if timeout_ms > MAX_TIMEOUT_MS as f64 {
        return err(ExecutionError::new(
            ExecutionErrorCode::Timeout,
            format!("Invalid timeout: maximum is {MAX_TIMEOUT_SECONDS} seconds"),
        ));
    }
    ok(Some(timeout_ms as u64))
}

fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
}

fn temp_dir() -> String {
    std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string())
}

/// `resolvePath(cwd, path)`.
fn resolve_path(cwd: &str, path: &str) -> String {
    let mut normalized = path.to_string();
    if normalized == "~" {
        normalized = home_dir();
    } else if let Some(rest) = normalized.strip_prefix("~/") {
        normalized = Path::new(&home_dir()).join(rest).to_string_lossy().into_owned();
    } else if let Some(rest) = normalized.strip_prefix("file://") {
        normalized = rest.to_string();
    }
    let candidate = Path::new(&normalized);
    if candidate.is_absolute() {
        normalize_path(candidate)
    } else {
        normalize_path(&Path::new(cwd).join(candidate))
    }
}

/// Lexical normalization matching `path.resolve` (no symlink resolution, `..` folded away).
fn normalize_path(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut prefix = String::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix_component) => {
                prefix.push_str(&prefix_component.as_os_str().to_string_lossy());
            }
            std::path::Component::RootDir => prefix.push('/'),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
        }
    }
    let mut result = prefix;
    result.push_str(&parts.join("/"));
    if result.is_empty() {
        "/".to_string()
    } else {
        result
    }
}

fn file_kind_from_metadata(metadata: &std::fs::Metadata) -> Option<FileKind> {
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        Some(FileKind::Symlink)
    } else if file_type.is_dir() {
        Some(FileKind::Directory)
    } else if file_type.is_file() {
        Some(FileKind::File)
    } else {
        None
    }
}

fn file_info_from_metadata(path: &str, metadata: &std::fs::Metadata) -> Result<FileInfo, FileError> {
    let Some(kind) = file_kind_from_metadata(metadata) else {
        return err(FileError::new(FileErrorCode::Invalid, "Unsupported file type", Some(path.to_string())));
    };
    let name = Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mtime_ms = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as f64)
        .unwrap_or(0.0);
    ok(FileInfo { name, path: path.to_string(), kind, size: metadata.len(), mtime_ms })
}

fn to_file_error(error: &std::io::Error, fallback_path: Option<&str>) -> FileError {
    let path = fallback_path.map(str::to_string);
    let message = error.to_string();
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => FileErrorCode::NotFound,
        std::io::ErrorKind::PermissionDenied => FileErrorCode::PermissionDenied,
        std::io::ErrorKind::NotADirectory => FileErrorCode::NotDirectory,
        std::io::ErrorKind::IsADirectory => FileErrorCode::IsDirectory,
        std::io::ErrorKind::InvalidInput | std::io::ErrorKind::InvalidData => FileErrorCode::Invalid,
        std::io::ErrorKind::Unsupported => FileErrorCode::NotSupported,
        _ => match error.raw_os_error() {
            Some(13) => FileErrorCode::PermissionDenied,
            Some(20) => FileErrorCode::NotDirectory,
            Some(21) => FileErrorCode::IsDirectory,
            Some(22) => FileErrorCode::Invalid,
            Some(2) => FileErrorCode::NotFound,
            _ => FileErrorCode::Unknown,
        },
    };
    FileError::new(code, message, path).with_cause(error.to_string())
}

fn abort_result<T>(signal: Option<&AbortSignal>, path: Option<&str>) -> Option<Result<T, FileError>> {
    match signal {
        Some(signal) if signal.aborted() => Some(err(FileError::new(
            FileErrorCode::Aborted,
            "aborted",
            path.map(str::to_string),
        ))),
        _ => None,
    }
}

fn path_exists(path: &str) -> bool {
    std::fs::metadata(path).is_ok() || std::fs::symlink_metadata(path).is_ok()
}

/// `isLegacyWslBashPath(path)`.
pub fn is_legacy_wsl_bash_path(path: &str) -> bool {
    let normalized = path.replace('/', "\\").to_lowercase();
    let bytes = normalized.as_bytes();
    if bytes.len() < 4 || bytes[1] != b':' || bytes[2] != b'\\' {
        return false;
    }
    if !(bytes[0].is_ascii_alphabetic()) {
        return false;
    }
    let rest = &normalized[3..];
    let rest = rest.strip_prefix("windows\\").unwrap_or(rest);
    rest == "system32\\bash.exe" || rest == "sysnative\\bash.exe"
}

/// `ShellConfig`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellConfig {
    pub shell: String,
    pub args: Vec<String>,
    pub command_transport: Option<&'static str>,
}

/// `getBashShellConfig(shell)`.
pub fn get_bash_shell_config(shell: &str) -> ShellConfig {
    if is_legacy_wsl_bash_path(shell) {
        ShellConfig { shell: shell.to_string(), args: vec!["-s".to_string()], command_transport: Some("stdin") }
    } else {
        ShellConfig { shell: shell.to_string(), args: vec!["-c".to_string()], command_transport: None }
    }
}

/// `windowsTaskkillCandidates(env)`.
pub fn windows_taskkill_candidates(env: &std::collections::BTreeMap<String, String>) -> Vec<String> {
    let system_drive = env.get("SystemDrive").map(|drive| format!("{drive}\\"));
    let roots: Vec<Option<String>> = vec![
        env.get("SystemRoot").cloned(),
        env.get("SYSTEMROOT").cloned(),
        env.get("windir").cloned(),
        system_drive.map(|drive| format!("{drive}Windows")),
    ];
    let mut candidates: Vec<String> = Vec::new();
    for root in roots.into_iter().flatten() {
        for system_dir in ["System32", "Sysnative"] {
            let absolute = format!("{root}\\{system_dir}\\taskkill.exe");
            if !candidates.contains(&absolute) && Path::new(&absolute).exists() {
                candidates.push(absolute);
            }
        }
    }
    candidates.push("taskkill.exe".to_string());
    candidates
}

fn find_bash_on_path() -> Option<String> {
    let result = Command::new("which").arg("bash").output().ok()?;
    if !result.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&result.stdout);
    let first_match = stdout.lines().next()?.trim().to_string();
    if first_match.is_empty() || !path_exists(&first_match) {
        None
    } else {
        Some(first_match)
    }
}

/// `getShellConfig(customShellPath)`.
fn get_shell_config(custom_shell_path: Option<&str>) -> Result<ShellConfig, ExecutionError> {
    if let Some(custom) = custom_shell_path {
        if path_exists(custom) {
            return ok(get_bash_shell_config(custom));
        }
        return err(ExecutionError::new(
            ExecutionErrorCode::ShellUnavailable,
            format!("Custom shell path not found: {custom}"),
        ));
    }
    if cfg!(windows) {
        let program_files = std::env::var("ProgramFiles").ok();
        let program_files_x86 = std::env::var("ProgramFiles(x86)").ok();
        let mut candidates: Vec<String> = Vec::new();
        if let Some(program_files) = program_files {
            candidates.push(format!("{program_files}\\Git\\bin\\bash.exe"));
        }
        if let Some(program_files_x86) = program_files_x86 {
            candidates.push(format!("{program_files_x86}\\Git\\bin\\bash.exe"));
        }
        for candidate in &candidates {
            if path_exists(candidate) {
                return ok(get_bash_shell_config(candidate));
            }
        }
        if let Some(bash_on_path) = find_bash_on_path() {
            return ok(get_bash_shell_config(&bash_on_path));
        }
        let searched = candidates
            .iter()
            .map(|path| format!("  {path}"))
            .collect::<Vec<_>>()
            .join("\n");
        return err(ExecutionError::new(
            ExecutionErrorCode::ShellUnavailable,
            format!(
                "No bash shell found. Options:\n  1. Install Git for Windows: https://git-scm.com/download/win\n  2. Add your bash to PATH (Cygwin, MSYS2, etc.)\n  3. Configure an explicit shellPath\n\nSearched Git Bash in:\n{searched}"
            ),
        ));
    }

    if path_exists("/bin/bash") {
        return ok(get_bash_shell_config("/bin/bash"));
    }
    if let Some(bash_on_path) = find_bash_on_path() {
        return ok(get_bash_shell_config(&bash_on_path));
    }
    ok(ShellConfig { shell: "sh".to_string(), args: vec!["-c".to_string()], command_transport: None })
}

/// `getShellEnv(baseEnv, extraEnv, inheritEnv)`.
fn get_shell_env(
    base_env: Option<&std::collections::BTreeMap<String, String>>,
    extra_env: Option<&serde_json::Map<String, serde_json::Value>>,
    inherit_env: bool,
) -> std::collections::BTreeMap<String, String> {
    let mut result = std::collections::BTreeMap::new();
    if inherit_env {
        for (key, value) in std::env::vars() {
            result.insert(key, value);
        }
    }
    if let Some(base_env) = base_env {
        for (key, value) in base_env {
            result.insert(key.clone(), value.clone());
        }
    }
    if let Some(extra_env) = extra_env {
        for (key, value) in extra_env {
            if let Some(value) = value.as_str() {
                result.insert(key.clone(), value.to_string());
            }
        }
    }
    result
}

/// `killProcessTree(pid)`.
fn kill_process_tree(pid: u32) {
    let script = format!("kill -9 -{pid} 2>/dev/null || kill -9 {pid} 2>/dev/null || true");
    let _ = Command::new("sh").arg("-c").arg(script).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

/// `killWindowsProcessTree(pid, taskkillPaths)`.
pub fn kill_windows_process_tree(pid: u32, taskkill_paths: &[String]) {
    for taskkill_path in taskkill_paths {
        let handled = Command::new(taskkill_path)
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.code().is_some())
            .unwrap_or(false);
        if handled {
            return;
        }
    }
    kill_process_tree(pid);
}

/// Strict LF reader; the final line's termination is reported.
pub struct NodeTextLineReader {
    file: Mutex<std::fs::File>,
    path: String,
    state: Mutex<ReaderState>,
}

struct ReaderState {
    buffered: String,
    pending: Vec<u8>,
    ended: bool,
    closed: bool,
}

impl NodeTextLineReader {
    fn new(file: std::fs::File, path: String) -> Self {
        Self {
            file: Mutex::new(file),
            path,
            state: Mutex::new(ReaderState { buffered: String::new(), pending: Vec::new(), ended: false, closed: false }),
        }
    }
}

impl TextLineReader for NodeTextLineReader {
    fn read_line<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, Result<Option<TextLine>, FileError>> {
        Box::pin(async move {
            if let Some(aborted) = abort_result::<Option<TextLine>>(context.abort_signal().as_ref(), Some(&self.path)) {
                return aborted;
            }
            if self.state.lock().expect("line reader poisoned").closed {
                return err(FileError::new(FileErrorCode::Invalid, "Text line reader is closed", Some(self.path.clone())));
            }

            loop {
                {
                    let mut state = self.state.lock().expect("line reader poisoned");
                    if let Some(newline) = state.buffered.find('\n') {
                        let text = state.buffered[..newline].to_string();
                        state.buffered = state.buffered[newline + 1..].to_string();
                        return ok(Some(TextLine { text, terminated: true }));
                    }
                    if state.ended {
                        if state.buffered.is_empty() {
                            return ok(None);
                        }
                        let text = std::mem::take(&mut state.buffered);
                        return ok(Some(TextLine { text, terminated: false }));
                    }
                }

                let mut chunk = [0u8; 64 * 1024];
                let read = {
                    let mut file = self.file.lock().expect("line reader poisoned");
                    file.read(&mut chunk)
                };
                if let Some(aborted) = abort_result::<Option<TextLine>>(context.abort_signal().as_ref(), Some(&self.path))
                {
                    return aborted;
                }
                let mut state = self.state.lock().expect("line reader poisoned");
                match read {
                    Ok(0) => {
                        state.ended = true;
                        let tail = decode_chunk(&mut state.pending, true);
                        state.buffered.push_str(&tail);
                    }
                    Ok(count) => {
                        let tail = decode_chunk_with(&mut state.pending, &chunk[..count], false);
                        state.buffered.push_str(&tail);
                    }
                    Err(error) => {
                        drop(state);
                        return err(to_file_error(&error, Some(&self.path)));
                    }
                }
            }
        })
    }

    fn close<'a>(&'a self, _context: &'a Context) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let mut state = self.state.lock().expect("line reader poisoned");
            if state.closed {
                return;
            }
            state.closed = true;
            state.buffered.clear();
        })
    }
}

fn decode_chunk(pending: &mut Vec<u8>, flush: bool) -> String {
    drain_utf8(pending, flush)
}

fn decode_chunk_with(pending: &mut Vec<u8>, chunk: &[u8], flush: bool) -> String {
    pending.extend_from_slice(chunk);
    drain_utf8(pending, flush)
}

fn drain_utf8(pending: &mut Vec<u8>, flush: bool) -> String {
    let mut output = String::new();
    loop {
        match std::str::from_utf8(pending) {
            Ok(text) => {
                output.push_str(text);
                pending.clear();
                return output;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                output.push_str(&String::from_utf8_lossy(&pending[..valid]));
                match error.error_len() {
                    None => {
                        if flush {
                            output.push('\u{fffd}');
                            pending.clear();
                        } else {
                            pending.drain(..valid);
                        }
                        return output;
                    }
                    Some(length) => {
                        output.push('\u{fffd}');
                        pending.drain(..valid + length);
                    }
                }
            }
        }
    }
}

fn random_uuid() -> String {
    let mut bytes = [0u8; 16];
    let filled = std::fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut bytes)).is_ok();
    if !filled {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        bytes[..16].copy_from_slice(&now.to_le_bytes());
        let counter = std::process::id();
        bytes[12..16].copy_from_slice(&counter.to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// `NodeExecutionEnv implements ExecutionEnv`.
pub struct NodeExecutionEnv {
    cwd: String,
    shell_path: Option<String>,
    shell_env: Option<std::collections::BTreeMap<String, String>>,
    active_child_pids: Arc<Mutex<Vec<u32>>>,
}

impl NodeExecutionEnv {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self { cwd: cwd.into(), shell_path: None, shell_env: None, active_child_pids: Arc::new(Mutex::new(Vec::new())) }
    }

    pub fn with_shell_path(mut self, shell_path: Option<String>) -> Self {
        self.shell_path = shell_path;
        self
    }

    pub fn with_shell_env(mut self, shell_env: Option<std::collections::BTreeMap<String, String>>) -> Self {
        self.shell_env = shell_env;
        self
    }

    fn resolved(&self, path: &str) -> String {
        resolve_path(&self.cwd, path)
    }
}

impl FileSystem for NodeExecutionEnv {
    fn cwd(&self) -> &str {
        &self.cwd
    }

    fn absolute_path<'a>(&'a self, path: &'a str, _context: &'a Context) -> BoxFuture<'a, Result<String, FileError>> {
        let resolved = self.resolved(path);
        Box::pin(async move { ok(resolved) })
    }

    fn join_path<'a>(&'a self, parts: Vec<String>, _context: &'a Context) -> BoxFuture<'a, Result<String, FileError>> {
        Box::pin(async move {
            let joined = parts.iter().fold(PathBuf::new(), |acc, part| acc.join(part));
            ok(joined.to_string_lossy().into_owned())
        })
    }

    fn read_text_file<'a>(&'a self, path: &'a str, context: &'a Context) -> BoxFuture<'a, Result<String, FileError>> {
        let resolved = self.resolved(path);
        let signal = context.abort_signal();
        Box::pin(async move {
            if let Some(aborted) = abort_result::<String>(signal.as_ref(), Some(&resolved)) {
                return aborted;
            }
            match std::fs::read_to_string(&resolved) {
                Ok(content) => ok(content),
                Err(error) => err(to_file_error(&error, Some(&resolved))),
            }
        })
    }

    fn open_text_line_reader<'a>(
        &'a self,
        path: &'a str,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Box<dyn TextLineReader>, FileError>> {
        let resolved = self.resolved(path);
        let signal = context.abort_signal();
        Box::pin(async move {
            if let Some(aborted) = abort_result::<Box<dyn TextLineReader>>(signal.as_ref(), Some(&resolved)) {
                return aborted;
            }
            match std::fs::File::open(&resolved) {
                Ok(file) => {
                    if let Some(aborted) = abort_result::<Box<dyn TextLineReader>>(signal.as_ref(), Some(&resolved)) {
                        return aborted;
                    }
                    ok(Box::new(NodeTextLineReader::new(file, resolved)) as Box<dyn TextLineReader>)
                }
                Err(error) => err(to_file_error(&error, Some(&resolved))),
            }
        })
    }

    fn read_text_lines<'a>(
        &'a self,
        path: &'a str,
        max_lines: Option<u64>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<String>, FileError>> {
        Box::pin(async move {
            if max_lines.is_some_and(|max_lines| max_lines == 0) {
                return ok(Vec::new());
            }
            let reader = match self.open_text_line_reader(path, context).await {
                Ok(reader) => reader,
                Err(error) => return err(error),
            };
            let mut lines = Vec::new();
            loop {
                if max_lines.is_some_and(|max_lines| lines.len() as u64 >= max_lines) {
                    break;
                }
                match reader.read_line(context).await {
                    Ok(Some(line)) => lines.push(line.text),
                    Ok(None) => break,
                    Err(error) => {
                        reader.close(context).await;
                        return err(error);
                    }
                }
            }
            reader.close(context).await;
            ok(lines)
        })
    }

    fn read_binary_file<'a>(&'a self, path: &'a str, context: &'a Context) -> BoxFuture<'a, Result<Vec<u8>, FileError>> {
        let resolved = self.resolved(path);
        let signal = context.abort_signal();
        Box::pin(async move {
            if let Some(aborted) = abort_result::<Vec<u8>>(signal.as_ref(), Some(&resolved)) {
                return aborted;
            }
            match std::fs::read(&resolved) {
                Ok(bytes) => ok(bytes),
                Err(error) => err(to_file_error(&error, Some(&resolved))),
            }
        })
    }

    fn write_file<'a>(
        &'a self,
        path: &'a str,
        content: &'a [u8],
        context: &'a Context,
    ) -> BoxFuture<'a, Result<(), FileError>> {
        let resolved = self.resolved(path);
        let signal = context.abort_signal();
        Box::pin(async move {
            if let Some(aborted) = abort_result::<()>(signal.as_ref(), Some(&resolved)) {
                return aborted;
            }
            if let Some(parent) = Path::new(&resolved).parent()
                && let Err(error) = std::fs::create_dir_all(parent)
            {
                return err(to_file_error(&error, Some(&resolved)));
            }
            if let Some(aborted) = abort_result::<()>(signal.as_ref(), Some(&resolved)) {
                return aborted;
            }
            match std::fs::write(&resolved, content) {
                Ok(()) => ok(()),
                Err(error) => err(to_file_error(&error, Some(&resolved))),
            }
        })
    }

    fn append_file<'a>(
        &'a self,
        path: &'a str,
        content: &'a [u8],
        context: &'a Context,
    ) -> BoxFuture<'a, Result<(), FileError>> {
        let resolved = self.resolved(path);
        let signal = context.abort_signal();
        Box::pin(async move {
            if let Some(aborted) = abort_result::<()>(signal.as_ref(), Some(&resolved)) {
                return aborted;
            }
            if let Some(parent) = Path::new(&resolved).parent()
                && let Err(error) = std::fs::create_dir_all(parent)
            {
                return err(to_file_error(&error, Some(&resolved)));
            }
            if let Some(aborted) = abort_result::<()>(signal.as_ref(), Some(&resolved)) {
                return aborted;
            }
            let result = std::fs::OpenOptions::new().append(true).create(true).open(&resolved).and_then(|mut file| {
                file.write_all(content)
            });
            match result {
                Ok(()) => match abort_result::<()>(signal.as_ref(), Some(&resolved)) {
                    Some(aborted) => aborted,
                    None => ok(()),
                },
                Err(error) => err(to_file_error(&error, Some(&resolved))),
            }
        })
    }

    fn rename_file<'a>(
        &'a self,
        source_path: &'a str,
        destination_path: &'a str,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<(), FileError>> {
        let source = self.resolved(source_path);
        let destination = self.resolved(destination_path);
        let signal = context.abort_signal();
        Box::pin(async move {
            if let Some(aborted) = abort_result::<()>(signal.as_ref(), Some(&destination)) {
                return aborted;
            }
            match std::fs::rename(&source, &destination) {
                Ok(()) => ok(()),
                Err(error) => err(to_file_error(&error, Some(&source))),
            }
        })
    }

    fn file_info<'a>(&'a self, path: &'a str, context: &'a Context) -> BoxFuture<'a, Result<FileInfo, FileError>> {
        let resolved = self.resolved(path);
        let signal = context.abort_signal();
        Box::pin(async move {
            if let Some(aborted) = abort_result::<FileInfo>(signal.as_ref(), Some(&resolved)) {
                return aborted;
            }
            match std::fs::symlink_metadata(&resolved) {
                Ok(metadata) => file_info_from_metadata(&resolved, &metadata),
                Err(error) => err(to_file_error(&error, Some(&resolved))),
            }
        })
    }

    fn list_dir<'a>(&'a self, path: &'a str, context: &'a Context) -> BoxFuture<'a, Result<Vec<FileInfo>, FileError>> {
        let resolved = self.resolved(path);
        let signal = context.abort_signal();
        Box::pin(async move {
            if let Some(aborted) = abort_result::<Vec<FileInfo>>(signal.as_ref(), Some(&resolved)) {
                return aborted;
            }
            let entries = match std::fs::read_dir(&resolved) {
                Ok(entries) => entries,
                Err(error) => return err(to_file_error(&error, Some(&resolved))),
            };
            let mut infos = Vec::new();
            for entry in entries {
                if let Some(aborted) = abort_result::<Vec<FileInfo>>(signal.as_ref(), Some(&resolved)) {
                    return aborted;
                }
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => return err(to_file_error(&error, Some(&resolved))),
                };
                let entry_path = entry.path().to_string_lossy().into_owned();
                match std::fs::symlink_metadata(&entry_path) {
                    Ok(metadata) => {
                        if let Ok(info) = file_info_from_metadata(&entry_path, &metadata) {
                            infos.push(info);
                        }
                    }
                    Err(error) => return err(to_file_error(&error, Some(&entry_path))),
                }
            }
            ok(infos)
        })
    }

    fn canonical_path<'a>(&'a self, path: &'a str, context: &'a Context) -> BoxFuture<'a, Result<String, FileError>> {
        let resolved = self.resolved(path);
        let signal = context.abort_signal();
        Box::pin(async move {
            if let Some(aborted) = abort_result::<String>(signal.as_ref(), Some(&resolved)) {
                return aborted;
            }
            match std::fs::canonicalize(&resolved) {
                Ok(canonical) => ok(canonical.to_string_lossy().into_owned()),
                Err(error) => err(to_file_error(&error, Some(&resolved))),
            }
        })
    }

    fn exists<'a>(&'a self, path: &'a str, context: &'a Context) -> BoxFuture<'a, Result<bool, FileError>> {
        Box::pin(async move {
            match self.file_info(path, context).await {
                Ok(_) => ok(true),
                Err(error) if error.code == FileErrorCode::NotFound => ok(false),
                Err(error) => err(error),
            }
        })
    }

    fn create_dir<'a>(
        &'a self,
        path: &'a str,
        recursive: Option<bool>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<(), FileError>> {
        let resolved = self.resolved(path);
        let signal = context.abort_signal();
        Box::pin(async move {
            if let Some(aborted) = abort_result::<()>(signal.as_ref(), Some(&resolved)) {
                return aborted;
            }
            let result = if recursive.unwrap_or(true) {
                std::fs::create_dir_all(&resolved)
            } else {
                std::fs::create_dir(&resolved)
            };
            match result {
                Ok(()) => ok(()),
                Err(error) => err(to_file_error(&error, Some(&resolved))),
            }
        })
    }

    fn remove<'a>(
        &'a self,
        path: &'a str,
        recursive: Option<bool>,
        force: Option<bool>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<(), FileError>> {
        let resolved = self.resolved(path);
        let signal = context.abort_signal();
        Box::pin(async move {
            if let Some(aborted) = abort_result::<()>(signal.as_ref(), Some(&resolved)) {
                return aborted;
            }
            let force = force.unwrap_or(false);
            let metadata = std::fs::symlink_metadata(&resolved);
            let result = match metadata {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if force {
                        return ok(());
                    }
                    return err(to_file_error(&error, Some(&resolved)));
                }
                Err(error) => return err(to_file_error(&error, Some(&resolved))),
                Ok(metadata) => {
                    if metadata.is_dir() && !metadata.file_type().is_symlink() {
                        if recursive.unwrap_or(false) {
                            std::fs::remove_dir_all(&resolved)
                        } else {
                            std::fs::remove_dir(&resolved)
                        }
                    } else {
                        std::fs::remove_file(&resolved)
                    }
                }
            };
            match result {
                Ok(()) => ok(()),
                Err(error) if force && error.kind() == std::io::ErrorKind::NotFound => ok(()),
                Err(error) => err(to_file_error(&error, Some(&resolved))),
            }
        })
    }

    fn create_temp_dir<'a>(
        &'a self,
        prefix: Option<String>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<String, FileError>> {
        let signal = context.abort_signal();
        Box::pin(async move {
            if let Some(aborted) = abort_result::<String>(signal.as_ref(), None) {
                return aborted;
            }
            let prefix = prefix.unwrap_or_else(|| "tmp-".to_string());
            for _ in 0..100 {
                let candidate = Path::new(&temp_dir()).join(format!("{prefix}{}", random_uuid()));
                match std::fs::create_dir(&candidate) {
                    Ok(()) => return ok(candidate.to_string_lossy().into_owned()),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return err(to_file_error(&error, Some(&candidate.to_string_lossy()))),
                }
            }
            err(FileError::new(FileErrorCode::Unknown, "Failed to create a unique temporary directory", None))
        })
    }

    fn create_temp_file<'a>(
        &'a self,
        prefix: Option<String>,
        suffix: Option<String>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<String, FileError>> {
        Box::pin(async move {
            let dir = match self.create_temp_dir(Some("tmp-".to_string()), context).await {
                Ok(dir) => dir,
                Err(error) => return err(error),
            };
            let file_path = Path::new(&dir)
                .join(format!("{}{}{}", prefix.unwrap_or_default(), random_uuid(), suffix.unwrap_or_default()))
                .to_string_lossy()
                .into_owned();
            match std::fs::write(&file_path, "") {
                Ok(()) => ok(file_path),
                Err(error) => err(to_file_error(&error, Some(&file_path))),
            }
        })
    }

    fn cleanup<'a>(&'a self, _context: &'a Context) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let pids: Vec<u32> = {
                let mut active = self.active_child_pids.lock().expect("active pids poisoned");
                std::mem::take(&mut *active)
            };
            for pid in pids {
                kill_process_tree(pid);
            }
        })
    }
}

impl Shell for NodeExecutionEnv {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        options: Option<ShellExecOptions>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<ShellExecResult, ExecutionError>> {
        Box::pin(async move { self.exec_inner(command, options, context).await })
    }

    fn cleanup<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, ()> {
        FileSystem::cleanup(self, context)
    }
}

enum ExecOutcome {
    Exited(std::process::ExitStatus),
    TimedOut,
    Aborted,
}

impl NodeExecutionEnv {
    async fn exec_inner(
        &self,
        command: &str,
        options: Option<ShellExecOptions>,
        context: &Context,
    ) -> Result<ShellExecResult, ExecutionError> {
        let signal = context.abort_signal();
        if signal.as_ref().is_some_and(|signal| signal.aborted()) {
            return err(ExecutionError::new(ExecutionErrorCode::Aborted, "aborted"));
        }
        let timeout_ms = match resolve_timeout_ms(options.as_ref().and_then(|options| options.timeout)) {
            Ok(timeout_ms) => timeout_ms,
            Err(error) => return err(error),
        };
        let cwd = match options.as_ref().and_then(|options| options.cwd.clone()) {
            Some(cwd) => resolve_path(&self.cwd, &cwd),
            None => self.cwd.clone(),
        };
        let shell_config = match get_shell_config(self.shell_path.as_deref()) {
            Ok(shell_config) => shell_config,
            Err(error) => return err(error),
        };
        if !Path::new(&cwd).exists() {
            return err(ExecutionError::new(
                ExecutionErrorCode::SpawnError,
                format!("Working directory does not exist: {cwd}\nCannot execute bash commands."),
            ));
        }

        let capture_options = options.as_ref().and_then(|options| options.capture.clone());
        let capture = match OutputCapture::new(
            capture_options.as_ref(),
            context.clone(),
            OutputCaptureHandlers {
                on_update: options.as_ref().and_then(|options| options.on_update.clone()),
                on_error: Arc::new(|_message| {}),
            },
        ) {
            Ok(capture) => Arc::new(capture),
            Err(message) => return err(ExecutionError::new(ExecutionErrorCode::Unknown, message)),
        };
        let spill_enabled = capture_options.as_ref().and_then(|options| options.spill).unwrap_or(false);

        let command_from_stdin = shell_config.command_transport == Some("stdin");
        let mut process = Command::new(&shell_config.shell);
        if !command_from_stdin {
            process.arg(&shell_config.args[0]);
            process.arg(command);
        } else {
            for arg in &shell_config.args {
                process.arg(arg);
            }
        }
        process
            .current_dir(&cwd)
            .envs(get_shell_env(self.shell_env.as_ref(), options.as_ref().and_then(|options| options.env.as_ref()), options
                .as_ref()
                .and_then(|options| options.inherit_env)
                .unwrap_or(true)))
            .stdin(if command_from_stdin { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child: Child = match process.spawn() {
            Ok(child) => child,
            Err(error) => return err(ExecutionError::new(ExecutionErrorCode::SpawnError, error.to_string())),
        };
        let pid = child.id();
        self.active_child_pids.lock().expect("active pids poisoned").push(pid);

        if command_from_stdin
            && let Some(mut stdin) = child.stdin.take()
        {
            let _ = stdin.write_all(command.as_bytes());
        }

        let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let mut stdout_pipe = child.stdout.take();
        let mut stderr_pipe = child.stderr.take();
        let mut readers = Vec::new();
        if let Some(pipe) = stdout_pipe.take() {
            let tx = chunk_tx.clone();
            readers.push(std::thread::spawn(move || read_stream(pipe, tx)));
        }
        if let Some(pipe) = stderr_pipe.take() {
            let tx = chunk_tx.clone();
            readers.push(std::thread::spawn(move || read_stream(pipe, tx)));
        }
        drop(chunk_tx);

        let (status_tx, mut status_rx) = tokio::sync::oneshot::channel();
        let waiter = std::thread::spawn(move || {
            let status = child.wait();
            let _ = status_tx.send(status);
        });

        let mut spill = SpillState::new();
        let mut outcome = None;
        let timeout_future = async {
            match timeout_ms {
                Some(timeout_ms) => tokio::time::sleep(Duration::from_millis(timeout_ms)).await,
                None => std::future::pending::<()>().await,
            }
        };
        let abort_future = async {
            match signal.clone() {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(timeout_future);
        tokio::pin!(abort_future);

        loop {
            tokio::select! {
                chunk = chunk_rx.recv() => {
                    match chunk {
                        Some(chunk) => {
                            capture.push_bytes(chunk.as_slice());
                            if spill_enabled {
                                spill.push(chunk.as_slice(), capture.truncated());
                            }
                        }
                        None => {
                            if outcome.is_none() {
                                outcome = Some(ExecOutcome::Exited(wait_status(&mut status_rx).await));
                            }
                            break;
                        }
                    }
                }
                status = &mut status_rx => {
                    let status = status.ok().and_then(|inner| inner.ok()).unwrap_or_else(exit_status_fallback);
                    outcome = Some(ExecOutcome::Exited(status));
                    break;
                }
                () = &mut timeout_future => {
                    kill_process_tree(pid);
                    outcome = Some(ExecOutcome::TimedOut);
                    break;
                }
                () = &mut abort_future => {
                    kill_process_tree(pid);
                    outcome = Some(ExecOutcome::Aborted);
                    break;
                }
            }
        }

        // Drain whatever the readers produced before the pipe closed (bounded grace, as TS does).
        while let Ok(Some(chunk)) =
            tokio::time::timeout(Duration::from_millis(EXIT_STDIO_GRACE_MS), chunk_rx.recv()).await
        {
            capture.push_bytes(chunk.as_slice());
            if spill_enabled {
                spill.push(chunk.as_slice(), capture.truncated());
            }
        }

        for reader in readers {
            let _ = reader.join();
        }
        let _ = waiter.join();
        {
            let mut active = self.active_child_pids.lock().expect("active pids poisoned");
            if let Some(index) = active.iter().position(|active_pid| *active_pid == pid) {
                active.remove(index);
            }
        }
        spill.finish(&capture);
        capture.finish();
        capture.flush();
        capture.dispose();

        let outcome = match outcome {
            Some(outcome) => outcome,
            None => ExecOutcome::Exited(wait_status(&mut status_rx).await),
        };
        match outcome {
            ExecOutcome::TimedOut => {
                err(ExecutionError::new(
                    ExecutionErrorCode::Timeout,
                    format!("timeout:{}", options.as_ref().and_then(|options| options.timeout).unwrap_or(0.0)),
                ))
            }
            ExecOutcome::Aborted => err(ExecutionError::new(ExecutionErrorCode::Aborted, "aborted")),
            ExecOutcome::Exited(status) => {
                let output = capture.snapshot();
                let exit_code = status.code().unwrap_or(1);
                ok(ShellExecResult {
                    exit_code,
                    truncation: output.truncation,
                    spill_path: output.spill_path,
                    last_line_bytes: output.last_line_bytes,
                })
            }
        }
    }
}

fn read_stream(mut pipe: impl Read, tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>) {
    let mut buffer = [0u8; 8192];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                if tx.send(buffer[..count].to_vec()).is_err() {
                    break;
                }
            }
        }
    }
}

async fn wait_status(status_rx: &mut tokio::sync::oneshot::Receiver<std::io::Result<std::process::ExitStatus>>) -> std::process::ExitStatus {
    match std::future::poll_fn(|cx| std::pin::Pin::new(&mut *status_rx).poll(cx)).await {
        Ok(Ok(status)) => status,
        Ok(Err(_)) | Err(_) => exit_status_fallback(),
    }
}

fn exit_status_fallback() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(1 << 8)
}

/// Spill state: buffers chunks until the capture truncates, then streams them to a temp file.
struct SpillState {
    prefix: Vec<Vec<u8>>,
    path: Option<String>,
    started: bool,
}

impl SpillState {
    fn new() -> Self {
        Self { prefix: Vec::new(), path: None, started: false }
    }

    fn push(&mut self, chunk: &[u8], truncated: bool) {
        if chunk.is_empty() {
            return;
        }
        if self.started {
            self.append(chunk);
            return;
        }
        if truncated {
            let prefix = std::mem::take(&mut self.prefix);
            for buffered in prefix {
                self.append(&buffered);
            }
            self.append(chunk);
        } else {
            self.prefix.push(chunk.to_vec());
        }
    }

    fn append(&mut self, chunk: &[u8]) {
        if self.path.is_none() {
            let dir = Path::new(&temp_dir()).join(format!("tmp-{}", random_uuid()));
            if std::fs::create_dir_all(&dir).is_err() {
                return;
            }
            let path = dir.join(format!("pi-output-{}.log", random_uuid()));
            self.path = Some(path.to_string_lossy().into_owned());
            self.started = true;
        }
        let Some(path) = self.path.clone() else {
            return;
        };
        if let Ok(mut file) = std::fs::OpenOptions::new().append(true).create(true).open(&path) {
            let _ = file.write_all(chunk);
        }
    }

    fn finish(&mut self, capture: &OutputCapture) {
        if let Some(path) = self.path.clone() {
            capture.set_spill_path(&path);
        }
    }
}
