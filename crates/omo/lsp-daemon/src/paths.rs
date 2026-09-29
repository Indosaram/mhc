//! Port of `paths.ts`: per-version daemon state layout and endpoint derivation.

use std::fmt;
use std::path::PathBuf;

use crate::crypto::short_digest;
use crate::platform;
pub use crate::runtime_contract::{
    DaemonRuntime, DaemonRuntimeDefaults, Env, InvalidDaemonVersionError,
    InvalidRuntimeOverrideError, OMO_LSP_DAEMON_CLI, OMO_LSP_DAEMON_DIR, OMO_LSP_DAEMON_VERSION,
    resolve_daemon_runtime, validate_daemon_version,
};

const MAX_SOCKET_PATH_LENGTH: usize = 100;

/// Path grammar the platform uses (TS `path.posix` / `path.win32`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathFlavor {
    Posix,
    Win32,
}

impl PathFlavor {
    fn separator(self) -> char {
        match self {
            Self::Posix => '/',
            Self::Win32 => '\\',
        }
    }

    fn is_separator(self, value: char) -> bool {
        match self {
            Self::Posix => value == '/',
            Self::Win32 => value == '/' || value == '\\',
        }
    }

    /// Splits an absolute path into its root prefix; `None` when relative.
    fn root(self, value: &str) -> Option<String> {
        match self {
            Self::Posix => value.starts_with('/').then(|| "/".to_string()),
            Self::Win32 => {
                let bytes = value.as_bytes();
                if bytes.len() >= 3
                    && bytes[0].is_ascii_alphabetic()
                    && bytes[1] == b':'
                    && self.is_separator(char::from(bytes[2]))
                {
                    Some(format!("{}:\\", char::from(bytes[0])))
                } else if value.starts_with("\\\\") || value.starts_with('\\') {
                    Some("\\".to_string())
                } else {
                    None
                }
            }
        }
    }

    pub fn is_absolute(self, value: &str) -> bool {
        self.root(value).is_some()
    }

    /// TS `path.join` followed by normalization.
    pub fn join(self, parts: &[&str]) -> String {
        let joined = parts
            .iter()
            .filter(|part| !part.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(&self.separator().to_string());
        self.normalize(&joined)
    }

    /// TS `path.resolve` for an already absolute path (normalizes `.`/`..`).
    pub fn resolve(self, value: &str) -> String {
        self.normalize(value)
    }

    fn normalize(self, value: &str) -> String {
        let root = self.root(value);
        let rest = match &root {
            Some(root) => &value[root.len().min(value.len())..],
            None => value,
        };
        let mut segments: Vec<&str> = Vec::new();
        for segment in rest.split(|character| self.is_separator(character)) {
            match segment {
                "" | "." => {}
                ".." => {
                    if segments.last().is_some_and(|last| *last != "..") {
                        segments.pop();
                    } else if root.is_none() {
                        segments.push(segment);
                    }
                }
                other => segments.push(other),
            }
        }
        let body = segments.join(&self.separator().to_string());
        match root {
            Some(root) => format!("{root}{body}"),
            None if body.is_empty() => ".".to_string(),
            None => body,
        }
    }
}

/// Injectable platform identity (TS `DaemonPlatform`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonPlatform {
    pub flavor: PathFlavor,
    pub home_dir: String,
    pub tmp_dir: String,
    pub uid: Option<u32>,
    pub username: String,
}

impl DaemonPlatform {
    pub fn current() -> Self {
        Self {
            flavor: if cfg!(windows) {
                PathFlavor::Win32
            } else {
                PathFlavor::Posix
            },
            home_dir: platform::home_dir()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            tmp_dir: platform::tmp_dir().to_string_lossy().into_owned(),
            uid: platform::current_uid(),
            username: platform::username().unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidDaemonDirectoryError {
    pub directory: String,
}

impl InvalidDaemonDirectoryError {
    pub const CODE: &'static str = "invalid_daemon_directory";
}

impl fmt::Display for InvalidDaemonDirectoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{OMO_LSP_DAEMON_DIR} must be an absolute path")
    }
}

impl std::error::Error for InvalidDaemonDirectoryError {}

/// Failures while deriving daemon paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonPathsError {
    Runtime(InvalidRuntimeOverrideError),
    Directory(InvalidDaemonDirectoryError),
}

impl fmt::Display for DaemonPathsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => error.fmt(formatter),
            Self::Directory(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for DaemonPathsError {}

/// Every per-version daemon artifact path (TS `DaemonPaths`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonPaths {
    pub version: String,
    pub cli_path: PathBuf,
    pub dir: PathBuf,
    pub socket: PathBuf,
    pub lock: PathBuf,
    pub pid: PathBuf,
    pub auth: PathBuf,
    pub endpoint: PathBuf,
    pub owner: PathBuf,
    pub log: PathBuf,
}

impl DaemonPaths {
    /// Test/QA helper equivalent to `daemonTestPaths`: all artifacts under `dir`.
    pub fn under_dir(dir: impl Into<PathBuf>, version: &str) -> Self {
        let dir = dir.into();
        Self {
            version: version.to_string(),
            cli_path: dir.join("cli.js"),
            socket: dir.join("daemon.sock"),
            lock: dir.join("daemon.lock"),
            pid: dir.join("daemon.pid"),
            auth: dir.join("daemon.auth"),
            endpoint: dir.join("daemon.endpoint"),
            owner: dir.join("daemon.owner"),
            log: dir.join("daemon.log"),
            dir,
        }
    }
}

/// Version stamped into this binary (TS `resolveDaemonVersion` reading package.json).
pub fn resolve_daemon_version() -> String {
    validate_daemon_version(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| "0".to_string())
}

/// Runtime defaults for this build: the running executable and the stamped version.
pub fn packaged_runtime_defaults() -> DaemonRuntimeDefaults {
    DaemonRuntime {
        cli_path: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("lsp-daemon")),
        version: resolve_daemon_version(),
    }
}

pub fn daemon_base_dir(
    env: &Env,
    platform: &DaemonPlatform,
) -> Result<String, InvalidDaemonDirectoryError> {
    if let Some(override_dir) = env.get(OMO_LSP_DAEMON_DIR) {
        if !platform.flavor.is_absolute(override_dir) {
            return Err(InvalidDaemonDirectoryError {
                directory: override_dir.clone(),
            });
        }
        return Ok(platform.flavor.resolve(override_dir));
    }
    Ok(platform
        .flavor
        .join(&[&platform.home_dir, ".maho", "lsp-daemon"]))
}

pub fn daemon_paths_with(
    env: &Env,
    runtime_defaults: &DaemonRuntimeDefaults,
    platform: &DaemonPlatform,
) -> Result<DaemonPaths, DaemonPathsError> {
    let runtime =
        resolve_daemon_runtime(env, runtime_defaults).map_err(DaemonPathsError::Runtime)?;
    let base_dir = daemon_base_dir(env, platform).map_err(DaemonPathsError::Directory)?;
    let flavor = platform.flavor;
    let dir = flavor.join(&[&base_dir, &format!("v{}", runtime.version)]);
    let file = |name: &str| PathBuf::from(flavor.join(&[&dir, name]));
    Ok(DaemonPaths {
        socket: PathBuf::from(resolve_socket_path(&dir, &runtime.version, platform)),
        lock: file("daemon.lock"),
        pid: file("daemon.pid"),
        auth: file("daemon.auth"),
        endpoint: file("daemon.endpoint"),
        owner: file("daemon.owner"),
        log: file("daemon.log"),
        version: runtime.version,
        cli_path: runtime.cli_path,
        dir: PathBuf::from(dir),
    })
}

/// Paths for this process: real environment, packaged runtime, current platform.
pub fn daemon_paths() -> Result<DaemonPaths, DaemonPathsError> {
    daemon_paths_with(
        &crate::runtime_contract::process_env(),
        &packaged_runtime_defaults(),
        &DaemonPlatform::current(),
    )
}

fn resolve_socket_path(dir: &str, version: &str, platform: &DaemonPlatform) -> String {
    let flavor = platform.flavor;
    let canonical_version_dir = flavor.resolve(dir);
    match flavor {
        PathFlavor::Win32 => {
            let uid = platform
                .uid
                .map_or_else(|| "win".to_string(), |uid| uid.to_string());
            let discriminator = format!(
                "{uid}:{}:{}",
                platform.username,
                flavor.resolve(&platform.home_dir)
            );
            let digest = short_digest(&format!("{canonical_version_dir}\0{discriminator}"));
            format!("\\\\.\\pipe\\omo-lsp-{version}-{digest}")
        }
        PathFlavor::Posix => {
            let natural = flavor.join(&[&canonical_version_dir, "daemon.sock"]);
            if natural.len() < MAX_SOCKET_PATH_LENGTH {
                return natural;
            }
            flavor.join(&[
                &platform.tmp_dir,
                &format!("omo-lsp-{version}-{}", short_digest(&canonical_version_dir)),
                "daemon.sock",
            ])
        }
    }
}

#[cfg(test)]
#[path = "paths_tests.rs"]
mod tests;
