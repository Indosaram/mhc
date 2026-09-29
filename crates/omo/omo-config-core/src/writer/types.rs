use crate::issue::PathSegment;
use crate::loader::types::OmoConfigEnv;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsErrorKind {
    Exists,
    NotFound,
    CrossDevice,
    Other,
}

#[derive(Debug, Clone)]
pub struct FsError {
    pub kind: FsErrorKind,
    pub message: String,
}

impl FsError {
    pub fn new(kind: FsErrorKind, message: impl Into<String>) -> Self {
        FsError {
            kind,
            message: message.into(),
        }
    }

    pub fn other(message: impl Into<String>) -> Self {
        FsError::new(FsErrorKind::Other, message)
    }

    pub fn exists(message: impl Into<String>) -> Self {
        FsError::new(FsErrorKind::Exists, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        FsError::new(FsErrorKind::NotFound, message)
    }

    pub fn from_io(error: std::io::Error, path: &str) -> Self {
        use std::io::ErrorKind;
        let kind = match error.kind() {
            ErrorKind::AlreadyExists => FsErrorKind::Exists,
            ErrorKind::NotFound => FsErrorKind::NotFound,
            _ => {
                if error.raw_os_error() == Some(18) {
                    FsErrorKind::CrossDevice
                } else {
                    FsErrorKind::Other
                }
            }
        };
        FsError::new(kind, format!("{path}: {error}"))
    }
}

pub trait OmoConfigWriteFileSystem {
    fn copy(&self, source: &str, destination: &str) -> Result<(), FsError>;
    fn exists(&self, path: &str) -> bool;
    fn is_symbolic_link(&self, path: &str) -> Result<bool, FsError>;
    fn mkdirs(&self, path: &str) -> Result<(), FsError>;
    fn read(&self, path: &str) -> Result<String, FsError>;
    fn list_dir(&self, path: &str) -> Result<Vec<String>, FsError>;
    fn rename(&self, from: &str, to: &str) -> Result<(), FsError>;
    fn unlink(&self, path: &str) -> Result<(), FsError>;
    fn write_exclusive(&self, path: &str, content: &str) -> Result<(), FsError>;
    fn write(&self, path: &str, content: &str) -> Result<(), FsError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct StdWriteFileSystem;

impl OmoConfigWriteFileSystem for StdWriteFileSystem {
    fn copy(&self, source: &str, destination: &str) -> Result<(), FsError> {
        std::fs::copy(source, destination)
            .map(|_| ())
            .map_err(|error| FsError::from_io(error, source))
    }

    fn exists(&self, path: &str) -> bool {
        std::path::Path::new(path).exists()
    }

    fn is_symbolic_link(&self, path: &str) -> Result<bool, FsError> {
        std::fs::symlink_metadata(path)
            .map(|metadata| metadata.file_type().is_symlink())
            .map_err(|error| FsError::from_io(error, path))
    }

    fn mkdirs(&self, path: &str) -> Result<(), FsError> {
        std::fs::create_dir_all(path).map_err(|error| FsError::from_io(error, path))
    }

    fn read(&self, path: &str) -> Result<String, FsError> {
        std::fs::read_to_string(path).map_err(|error| FsError::from_io(error, path))
    }

    fn list_dir(&self, path: &str) -> Result<Vec<String>, FsError> {
        let entries = std::fs::read_dir(path).map_err(|error| FsError::from_io(error, path))?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| FsError::from_io(error, path))?;
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        Ok(names)
    }

    fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        std::fs::rename(from, to).map_err(|error| FsError::from_io(error, from))
    }

    fn unlink(&self, path: &str) -> Result<(), FsError> {
        std::fs::remove_file(path).map_err(|error| FsError::from_io(error, path))
    }

    fn write_exclusive(&self, path: &str, content: &str) -> Result<(), FsError> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| FsError::from_io(error, path))?;
        file.write_all(content.as_bytes())
            .map_err(|error| FsError::from_io(error, path))
    }

    fn write(&self, path: &str, content: &str) -> Result<(), FsError> {
        std::fs::write(path, content).map_err(|error| FsError::from_io(error, path))
    }
}

#[derive(Debug, Clone)]
pub struct OmoConfigEdit {
    pub path: Vec<PathSegment>,
    pub value: Option<serde_json::Value>,
}

impl OmoConfigEdit {
    pub fn set(path: Vec<PathSegment>, value: serde_json::Value) -> Self {
        OmoConfigEdit {
            path,
            value: Some(value),
        }
    }

    pub fn remove(path: Vec<PathSegment>) -> Self {
        OmoConfigEdit { path, value: None }
    }
}

#[derive(Default)]
pub struct UpdateOmoConfigOptions<'a> {
    pub edits: Vec<OmoConfigEdit>,
    pub env: Option<OmoConfigEnv>,
    pub file_system: Option<&'a dyn OmoConfigWriteFileSystem>,
    pub platform: Option<String>,
    pub project_dir: Option<String>,
    pub scope: &'a str,
    pub target_path: Option<String>,
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateOmoConfigResult {
    pub backup_path: Option<String>,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmoConfigWriteError {
    pub path: String,
    pub operation: &'static str,
    pub detail: String,
}

impl std::fmt::Display for OmoConfigWriteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Failed to {} omo config at {}: {}",
            self.operation, self.path, self.detail
        )
    }
}

impl std::error::Error for OmoConfigWriteError {}

impl OmoConfigWriteError {
    pub fn new(path: &str, operation: &'static str, detail: impl std::fmt::Display) -> Self {
        OmoConfigWriteError {
            path: path.to_string(),
            operation,
            detail: detail.to_string(),
        }
    }
}
