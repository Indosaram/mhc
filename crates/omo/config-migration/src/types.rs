use std::collections::BTreeMap;
use std::fmt;
use std::io::ErrorKind;

use crate::{path_posix, path_win32};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathOperations {
    Posix,
    Win32,
}

impl PathOperations {
    pub fn basename(self, path: &str) -> String {
        match self {
            PathOperations::Posix => path_posix::basename(path),
            PathOperations::Win32 => path_win32::basename(path),
        }
    }

    pub fn dirname(self, path: &str) -> String {
        match self {
            PathOperations::Posix => path_posix::dirname(path),
            PathOperations::Win32 => path_win32::dirname(path),
        }
    }

    pub fn is_absolute(self, path: &str) -> bool {
        match self {
            PathOperations::Posix => path_posix::is_absolute(path),
            PathOperations::Win32 => path_win32::is_absolute(path),
        }
    }

    pub fn join(self, paths: &[&str]) -> String {
        match self {
            PathOperations::Posix => path_posix::join(paths),
            PathOperations::Win32 => path_win32::join(paths),
        }
    }

    pub fn normalize(self, path: &str) -> String {
        match self {
            PathOperations::Posix => path_posix::normalize(path),
            PathOperations::Win32 => path_win32::normalize(path),
        }
    }

    pub fn relative(self, from: &str, to: &str) -> String {
        match self {
            PathOperations::Posix => path_posix::relative(from, to),
            PathOperations::Win32 => path_win32::relative(from, to),
        }
    }

    pub fn resolve(self, paths: &[&str]) -> String {
        match self {
            PathOperations::Posix => path_posix::resolve(paths),
            PathOperations::Win32 => path_win32::resolve(paths),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Darwin,
    Linux,
    Win32,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryFsErrorCode {
    NotFound,
    NotADirectory,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryFsError {
    pub code: DiscoveryFsErrorCode,
    pub message: String,
}

impl DiscoveryFsError {
    pub fn not_found(path: &str) -> Self {
        DiscoveryFsError {
            code: DiscoveryFsErrorCode::NotFound,
            message: format!("Missing {path}"),
        }
    }

    pub(crate) fn is_missing(&self) -> bool {
        matches!(
            self.code,
            DiscoveryFsErrorCode::NotFound | DiscoveryFsErrorCode::NotADirectory
        )
    }
}

impl From<std::io::Error> for DiscoveryFsError {
    fn from(error: std::io::Error) -> Self {
        let code = match error.kind() {
            ErrorKind::NotFound => DiscoveryFsErrorCode::NotFound,
            ErrorKind::NotADirectory => DiscoveryFsErrorCode::NotADirectory,
            _ => DiscoveryFsErrorCode::Other,
        };
        DiscoveryFsError {
            code,
            message: error.to_string(),
        }
    }
}

impl fmt::Display for DiscoveryFsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for DiscoveryFsError {}

pub trait ConfigMigrationDiscoveryFileSystem {
    fn exists(&self, path: &str) -> bool;
    fn read_dir(&self, path: &str) -> Result<Vec<String>, DiscoveryFsError>;
    fn realpath(&self, path: &str) -> Result<String, DiscoveryFsError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct StdDiscoveryFileSystem;

impl ConfigMigrationDiscoveryFileSystem for StdDiscoveryFileSystem {
    fn exists(&self, path: &str) -> bool {
        std::path::Path::new(path).exists()
    }

    fn read_dir(&self, path: &str) -> Result<Vec<String>, DiscoveryFsError> {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(path)? {
            entries.push(entry?.file_name().to_string_lossy().into_owned());
        }
        Ok(entries)
    }

    fn realpath(&self, path: &str) -> Result<String, DiscoveryFsError> {
        Ok(std::fs::canonicalize(path)?.to_string_lossy().into_owned())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyConfigSourceKind {
    ConfigJsonc,
    MigrationSidecar,
    ProfileConfig,
    ProjectConfig,
    UserConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredLegacyConfigSource {
    pub base_root: Option<String>,
    pub config_path: String,
    pub is_active_profile: bool,
    pub kind: LegacyConfigSourceKind,
    pub path: String,
    pub precedence: i64,
    pub profile: Option<String>,
    pub project_root: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyConfigMigrationGroup {
    pub id: String,
    pub sources: Vec<DiscoveredLegacyConfigSource>,
}

#[derive(Clone, Copy)]
pub struct ConfigMigrationDiscoveryOptions<'a> {
    pub cwd: &'a str,
    pub environment: &'a BTreeMap<String, String>,
    pub file_system: Option<&'a dyn ConfigMigrationDiscoveryFileSystem>,
    pub home_dir: &'a str,
    pub path_operations: PathOperations,
    pub platform: Option<Platform>,
    pub tauri_config_dirs: Option<&'a [String]>,
}
