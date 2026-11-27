//! Port of senpi `packages/agent/src/testing/win32-sim.ts`.
//!
//! Win32 simulation profile for code-level Windows tests on any host (test support only; no product code imports it).

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Win32ErrorCode {
    Ebusy,
    Eperm,
    Enosys,
}

impl Win32ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ebusy => "EBUSY",
            Self::Eperm => "EPERM",
            Self::Enosys => "ENOSYS",
        }
    }

    pub fn errno(self) -> i32 {
        match self {
            Self::Ebusy => -4082,
            Self::Eperm => -4048,
            Self::Enosys => -4054,
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Ebusy => "resource busy or locked",
            Self::Eperm => "operation not permitted",
            Self::Enosys => "function not implemented",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Win32OpenFileErrorCode {
    Ebusy,
    Eperm,
}

impl From<Win32OpenFileErrorCode> for Win32ErrorCode {
    fn from(value: Win32OpenFileErrorCode) -> Self {
        match value {
            Win32OpenFileErrorCode::Ebusy => Self::Ebusy,
            Win32OpenFileErrorCode::Eperm => Self::Eperm,
        }
    }
}

#[derive(Debug)]
pub struct Win32ErrnoException {
    pub message: String,
    pub code: Option<String>,
    pub errno: Option<i32>,
    pub syscall: Option<String>,
    pub path: Option<String>,
    pub dest: Option<String>,
}

impl fmt::Display for Win32ErrnoException {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Win32ErrnoException {}

/// Builds the error Node throws on Windows for `code` from `syscall`.
pub fn win32_error(
    code: Win32ErrorCode,
    syscall: &str,
    file_path: Option<&str>,
    dest: Option<&str>,
) -> Win32ErrnoException {
    let target = match (file_path, dest) {
        (Some(fp), None) => format!(" '{fp}'"),
        (Some(fp), Some(d)) => format!(" '{fp}' -> '{d}'"),
        (None, _) => String::new(),
    };
    let message = format!(
        "{}: {}, {syscall}{target}",
        code.as_str(),
        code.description()
    );
    Win32ErrnoException {
        message,
        code: Some(code.as_str().to_string()),
        errno: Some(code.errno()),
        syscall: Some(syscall.to_string()),
        path: file_path.map(str::to_string),
        dest: dest.map(str::to_string),
    }
}

/// Compares two Windows names (path segments, paths or pipe names) case-insensitively.
pub fn win32_names_equal(left: &str, right: &str) -> bool {
    left.to_uppercase() == right.to_uppercase()
}

fn normalize_win32_path(input: &str) -> String {
    let replaced = input.replace('/', "\\");
    let mut prefix = String::new();
    let rest = if replaced.len() >= 3
        && replaced.as_bytes()[0].is_ascii_alphabetic()
        && replaced.as_bytes()[1] == b':'
        && replaced.as_bytes()[2] == b'\\'
    {
        prefix = replaced[..3].to_string();
        &replaced[3..]
    } else {
        replaced.as_str()
    };
    let mut parts: Vec<&str> = Vec::new();
    for seg in rest.split('\\') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            if prefix.is_empty() || !parts.is_empty() {
                parts.pop();
            }
            continue;
        }
        parts.push(seg);
    }
    if prefix.is_empty() {
        parts.join("\\")
    } else {
        format!("{prefix}{}", parts.join("\\"))
    }
}

fn parse_drive_path(win32_path: &str) -> Option<(String, String)> {
    let norm = normalize_win32_path(win32_path);
    let bytes = norm.as_bytes();
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\' {
        Some((norm[..1].to_string(), norm[3..].to_string()))
    } else {
        None
    }
}

fn win32_basename(win32_path: &str) -> &str {
    let norm = win32_path.trim_end_matches(['\\', '/']);
    match norm.rfind(['\\', '/']) {
        Some(idx) => &norm[idx + 1..],
        None => norm,
    }
}

/// Turns a relative POSIX-style path (`dir/file`) into a drive-letter Windows path (`C:\dir\file`).
pub fn to_win32_path(relative_path: &str, drive: &str) -> Result<String, String> {
    if drive.len() != 1 || !drive.as_bytes()[0].is_ascii_alphabetic() {
        return Err(format!(
            "win32 sim: drive must be one letter, got {:?}",
            drive
        ));
    }
    if relative_path.starts_with('/') {
        return Err(format!(
            "win32 sim: expected a relative path, got {:?}",
            relative_path
        ));
    }
    let parts: Vec<&str> = relative_path
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    Ok(format!("{}:\\{}", drive.to_uppercase(), parts.join("\\")))
}

/// True for an absolute drive-letter path such as `C:\dir`.
pub fn is_win32_drive_path(candidate: &str) -> bool {
    parse_drive_path(candidate).is_some()
}

pub const WIN32_PIPE_PREFIX: &str = "\\\\.\\pipe\\";
const WIN32_PIPE_MAX_LENGTH: usize = 256;

/// Builds the named-pipe endpoint (`\\.\pipe\<name>`) a Windows transport listens on instead of a socket file.
pub fn win32_pipe_endpoint(name: &str) -> Result<String, String> {
    if name.is_empty() || name.contains('\\') {
        return Err(format!(
            "win32 sim: a pipe name must be non-empty and contain no backslash, got {:?}",
            name
        ));
    }
    let endpoint = format!("{WIN32_PIPE_PREFIX}{name}");
    if endpoint.len() > WIN32_PIPE_MAX_LENGTH {
        return Err(format!(
            "win32 sim: a pipe endpoint is at most {WIN32_PIPE_MAX_LENGTH} characters"
        ));
    }
    Ok(endpoint)
}

/// True when `endpoint` is a local named-pipe endpoint; prefix is matched case-insensitively.
pub fn is_win32_pipe_endpoint(endpoint: &str) -> bool {
    if endpoint.len() <= WIN32_PIPE_PREFIX.len() {
        return false;
    }
    let (prefix, name) = endpoint.split_at(WIN32_PIPE_PREFIX.len());
    win32_names_equal(prefix, WIN32_PIPE_PREFIX) && !name.is_empty() && !name.contains('\\')
}

#[derive(Debug, Clone)]
pub struct Win32SimFsOptions {
    pub root: PathBuf,
    pub rename_open_file_error: Win32OpenFileErrorCode,
    pub delete_open_file_error: Win32OpenFileErrorCode,
}

impl Win32SimFsOptions {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            rename_open_file_error: Win32OpenFileErrorCode::Eperm,
            delete_open_file_error: Win32OpenFileErrorCode::Ebusy,
        }
    }
}

#[derive(Debug)]
pub enum Win32FsError {
    Win32(Win32ErrnoException),
    Io(std::io::Error),
    Type(String),
}

impl fmt::Display for Win32FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Win32(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "{e}"),
            Self::Type(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for Win32FsError {}

impl Win32FsError {
    pub fn win32_code(&self) -> Option<&str> {
        match self {
            Self::Win32(e) => e.code.as_deref(),
            _ => None,
        }
    }
}

pub struct Win32SimFileHandle {
    backing: PathBuf,
    open_counts: std::sync::Arc<Mutex<BTreeMap<PathBuf, usize>>>,
    file: Option<tokio::fs::File>,
    closed: bool,
}

impl Win32SimFileHandle {
    pub fn file(&self) -> Option<&tokio::fs::File> {
        self.file.as_ref()
    }

    pub fn file_mut(&mut self) -> Option<&mut tokio::fs::File> {
        self.file.as_mut()
    }

    pub async fn close(&mut self) -> Result<(), Win32FsError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        {
            let mut guard = self.open_counts.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(count) = guard.get(&self.backing).copied() {
                if count <= 1 {
                    guard.remove(&self.backing);
                } else {
                    guard.insert(self.backing.clone(), count - 1);
                }
            }
        }
        self.file.take();
        Ok(())
    }
}

impl Drop for Win32SimFileHandle {
    fn drop(&mut self) {
        if !self.closed {
            self.closed = true;
            let mut guard = self.open_counts.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(count) = guard.get(&self.backing).copied() {
                if count <= 1 {
                    guard.remove(&self.backing);
                } else {
                    guard.insert(self.backing.clone(), count - 1);
                }
            }
        }
    }
}

pub struct Win32SimFs {
    root: PathBuf,
    rename_open_file_error: Win32OpenFileErrorCode,
    delete_open_file_error: Win32OpenFileErrorCode,
    open_counts: std::sync::Arc<Mutex<BTreeMap<PathBuf, usize>>>,
}

impl Win32SimFs {
    pub fn new(options: Win32SimFsOptions) -> Self {
        Self {
            root: options.root,
            rename_open_file_error: options.rename_open_file_error,
            delete_open_file_error: options.delete_open_file_error,
            open_counts: std::sync::Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub async fn resolve(&self, win32_path: &str) -> Result<PathBuf, Win32FsError> {
        let Some((drive, rest)) = parse_drive_path(win32_path) else {
            return Err(Win32FsError::Type(format!(
                "win32 sim: expected a drive-letter path such as C:\\dir\\file, got {:?}",
                win32_path
            )));
        };
        let segments: Vec<&str> = rest.split('\\').filter(|s| !s.is_empty()).collect();
        let mut current = self.root.join(drive.to_uppercase());
        for (index, segment) in segments.iter().enumerate() {
            match tokio::fs::read_dir(&current).await {
                Ok(mut rd) => {
                    let mut matched = None;
                    while let Some(entry) = rd.next_entry().await.map_err(Win32FsError::Io)? {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        if win32_names_equal(&name, segment) {
                            matched = Some(name);
                            break;
                        }
                    }
                    current = current.join(matched.as_deref().unwrap_or(segment));
                }
                Err(err)
                    if err.kind() == std::io::ErrorKind::NotFound
                        || err.kind() == std::io::ErrorKind::NotADirectory =>
                {
                    for rem in &segments[index..] {
                        current = current.join(rem);
                    }
                    return Ok(current);
                }
                Err(err) => return Err(Win32FsError::Io(err)),
            }
        }
        Ok(current)
    }

    pub async fn mkdir(&self, win32_path: &str) -> Result<(), Win32FsError> {
        let backing = self.resolve(win32_path).await?;
        tokio::fs::create_dir_all(backing)
            .await
            .map_err(Win32FsError::Io)
    }

    pub async fn write_file(&self, win32_path: &str, data: &[u8]) -> Result<(), Win32FsError> {
        let backing = self.resolve(win32_path).await?;
        tokio::fs::write(backing, data)
            .await
            .map_err(Win32FsError::Io)
    }

    pub async fn read_file(&self, win32_path: &str) -> Result<Vec<u8>, Win32FsError> {
        let backing = self.resolve(win32_path).await?;
        tokio::fs::read(backing).await.map_err(Win32FsError::Io)
    }

    pub async fn readdir(&self, win32_path: &str) -> Result<Vec<String>, Win32FsError> {
        let backing = self.resolve(win32_path).await?;
        let mut rd = tokio::fs::read_dir(backing)
            .await
            .map_err(Win32FsError::Io)?;
        let mut names = Vec::new();
        while let Some(entry) = rd.next_entry().await.map_err(Win32FsError::Io)? {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        Ok(names)
    }

    pub async fn exists(&self, win32_path: &str) -> Result<bool, Win32FsError> {
        let backing = self.resolve(win32_path).await?;
        match tokio::fs::metadata(backing).await {
            Ok(_) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(Win32FsError::Io(err)),
        }
    }

    pub async fn open_read(&self, win32_path: &str) -> Result<Win32SimFileHandle, Win32FsError> {
        let backing = self.resolve(win32_path).await?;
        let file = tokio::fs::File::open(&backing)
            .await
            .map_err(Win32FsError::Io)?;
        {
            let mut guard = self.open_counts.lock().unwrap_or_else(|p| p.into_inner());
            let next = guard.get(&backing).copied().unwrap_or(0) + 1;
            guard.insert(backing.clone(), next);
        }
        Ok(Win32SimFileHandle {
            backing,
            open_counts: self.open_counts.clone(),
            file: Some(file),
            closed: false,
        })
    }

    pub async fn rename(&self, from: &str, to: &str) -> Result<(), Win32FsError> {
        let source = self.resolve(from).await?;
        let mut target = self.resolve(to).await?;
        if self.is_open(&source) || self.is_open(&target) {
            return Err(Win32FsError::Win32(win32_error(
                self.rename_open_file_error.into(),
                "rename",
                Some(from),
                Some(to),
            )));
        }
        if target == source {
            let norm_to = normalize_win32_path(to);
            let base = win32_basename(&norm_to);
            if let Some(parent) = source.parent() {
                target = parent.join(base);
            }
        }
        tokio::fs::rename(source, target)
            .await
            .map_err(Win32FsError::Io)
    }

    pub async fn unlink(&self, win32_path: &str) -> Result<(), Win32FsError> {
        let backing = self.resolve(win32_path).await?;
        if self.is_open(&backing) {
            return Err(Win32FsError::Win32(win32_error(
                self.delete_open_file_error.into(),
                "unlink",
                Some(win32_path),
                None,
            )));
        }
        tokio::fs::remove_file(backing)
            .await
            .map_err(Win32FsError::Io)
    }

    pub async fn rm(&self, win32_path: &str) -> Result<(), Win32FsError> {
        let backing = self.resolve(win32_path).await?;
        {
            let guard = self.open_counts.lock().unwrap_or_else(|p| p.into_inner());
            for open in guard.keys() {
                if open == &backing || open.starts_with(&backing) {
                    return Err(Win32FsError::Win32(win32_error(
                        self.delete_open_file_error.into(),
                        "rm",
                        Some(win32_path),
                        None,
                    )));
                }
            }
        }
        tokio::fs::remove_dir_all(backing)
            .await
            .map_err(Win32FsError::Io)
    }

    fn is_open(&self, backing: &Path) -> bool {
        let guard = self.open_counts.lock().unwrap_or_else(|p| p.into_inner());
        guard.contains_key(backing)
    }
}

#[cfg(unix)]
#[allow(clippy::result_large_err)]
pub fn win32_kill_pid(pid: i32, signal: Option<&str>) -> Result<(), Win32ErrnoException> {
    let sig_name = signal.unwrap_or("SIGTERM");
    let sig_flag = match sig_name {
        "0" => "-0",
        "SIGKILL" | "SIGTERM" | "SIGINT" | "SIGQUIT" => "-KILL",
        _ => return Err(win32_error(Win32ErrorCode::Enosys, "kill", None, None)),
    };
    let output = std::process::Command::new("kill")
        .arg(sig_flag)
        .arg(pid.to_string())
        .output()
        .map_err(|err| Win32ErrnoException {
            message: err.to_string(),
            code: err.raw_os_error().map(|c| format!("ERRNO_{c}")),
            errno: err.raw_os_error(),
            syscall: Some("kill".to_string()),
            path: None,
            dest: None,
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(Win32ErrnoException {
            message: if stderr.is_empty() {
                format!("kill {sig_flag} {pid} failed")
            } else {
                stderr
            },
            code: Some("ESRCH".to_string()),
            errno: None,
            syscall: Some("kill".to_string()),
            path: None,
            dest: None,
        });
    }
    Ok(())
}
