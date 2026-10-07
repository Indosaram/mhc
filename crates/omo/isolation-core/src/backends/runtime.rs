use std::path::{Path, PathBuf};
use std::process::Command;

use crate::backend::{platform_name, IsolationError, Result};
use crate::git::command::exists;
use crate::util::{device_of, dirname};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub trait BackendRuntime: Send + Sync {
    fn platform(&self) -> &str;
    fn which(&self, binary: &str) -> bool;
    fn run(&self, argv: &[String]) -> Result<CommandResult>;
    fn device(&self, path: &Path) -> Result<u64>;
    fn accessible(&self, path: &Path) -> Result<bool>;
    fn mounted(&self, path: &Path) -> Result<bool>;
    fn wait_mounted(&self, path: &Path) -> Result<()>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemRuntime;

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(metadata) => metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(metadata) => metadata.is_file(),
        Err(_) => false,
    }
}

fn decode_mount_path(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 3 < bytes.len() {
            let digits = &raw[index + 1..index + 4];
            if let Ok(value) = u8::from_str_radix(digits, 8) {
                out.push(value);
                index += 4;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl BackendRuntime for SystemRuntime {
    fn platform(&self) -> &str {
        platform_name()
    }

    fn which(&self, binary: &str) -> bool {
        let path = std::env::var("PATH").unwrap_or_default();
        let suffixes: &[&str] = if cfg!(windows) {
            &["", ".exe", ".cmd"]
        } else {
            &[""]
        };
        for dir in std::env::split_paths(&path) {
            for suffix in suffixes {
                if is_executable(&dir.join(format!("{binary}{suffix}"))) {
                    return true;
                }
            }
        }
        false
    }

    fn run(&self, argv: &[String]) -> Result<CommandResult> {
        let Some((program, rest)) = argv.split_first() else {
            return Err(IsolationError::other("empty command"));
        };
        match Command::new(program).args(rest).output() {
            Ok(output) => Ok(CommandResult {
                code: output.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(IsolationError::unavailable(format!("{program} not on PATH")))
            }
            Err(error) => Err(error.into()),
        }
    }

    fn device(&self, path: &Path) -> Result<u64> {
        Ok(device_of(&std::fs::metadata(path)?))
    }

    fn accessible(&self, path: &Path) -> Result<bool> {
        #[cfg(unix)]
        {
            use std::ffi::CString;
            use std::os::unix::ffi::OsStrExt;
            let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
                return Ok(false);
            };
            if unsafe { libc::access(c_path.as_ptr(), libc::R_OK | libc::W_OK) } == 0 {
                return Ok(true);
            }
            match std::io::Error::last_os_error().raw_os_error() {
                Some(code)
                    if code == libc::ENOENT || code == libc::EACCES || code == libc::EPERM =>
                {
                    Ok(false)
                }
                Some(_) => Ok(false),
                None => Ok(false),
            }
        }
        #[cfg(not(unix))]
        {
            match std::fs::metadata(path) {
                Ok(_) => Ok(true),
                Err(error) if is_denied(&error) => Ok(false),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(error.into()),
            }
        }
    }

    fn mounted(&self, path: &Path) -> Result<bool> {
        #[cfg(target_os = "linux")]
        {
            let mounts = std::fs::read_to_string("/proc/mounts")?;
            let target = path.to_string_lossy();
            Ok(mounts.lines().any(|line| {
                line.split(' ')
                    .nth(1)
                    .map(|raw| decode_mount_path(raw) == target)
                    .unwrap_or(false)
            }))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = path;
            Ok(false)
        }
    }

    fn wait_mounted(&self, path: &Path) -> Result<()> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(5000);
        loop {
            if self.mounted(path)? {
                return Ok(());
            }
            // External kernel state has no notification API; only backend mount readiness polls.
            if std::time::Instant::now() >= deadline {
                return Err(IsolationError::other(format!(
                    "Mount did not appear within 5 seconds: {}",
                    path.display()
                )));
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
}

pub fn existing_parent(path: &Path) -> Result<PathBuf> {
    let mut current = path.to_path_buf();
    while !exists(&current)? {
        let parent = dirname(&current);
        if parent == current {
            return Err(IsolationError::other(format!(
                "No existing parent for {}",
                path.display()
            )));
        }
        current = parent;
    }
    Ok(current)
}

pub fn checked(io: &dyn BackendRuntime, argv: &[String]) -> Result<CommandResult> {
    let result = io.run(argv)?;
    if result.code != 0 {
        return Err(IsolationError::other(format!(
            "{} exited {}: {}",
            argv.first().cloned().unwrap_or_default(),
            result.code,
            result.stderr
        )));
    }
    Ok(result)
}
