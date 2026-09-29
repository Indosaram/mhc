use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::internal::posix_path::to_posix_path;

pub const DIAGNOSTIC_PARSE: &str = "parse";
pub const DIAGNOSTIC_PROFILE: &str = "profile";
pub const DIAGNOSTIC_READ: &str = "read";
pub const DIAGNOSTIC_VALIDATION: &str = "validation";

pub const SCOPE_PROJECT: &str = "project";
pub const SCOPE_USER: &str = "user";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmoConfigDiagnostic {
    pub kind: &'static str,
    pub path: String,
    pub message: String,
    pub issue_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmoConfigSource {
    pub exists: bool,
    pub loaded: bool,
    pub path: String,
    pub scope: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OmoConfigRawLayer {
    pub config: Value,
    pub source: OmoConfigSource,
}

pub type OmoConfigEnv = BTreeMap<String, String>;

pub trait OmoConfigReadFileSystem {
    fn exists(&self, path: &str) -> bool;
    fn read(&self, path: &str) -> std::io::Result<String>;
    fn is_symbolic_link(&self, path: &str) -> Option<bool>;
    fn realpath(&self, path: &str) -> Option<String>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct StdReadFileSystem;

impl OmoConfigReadFileSystem for StdReadFileSystem {
    fn exists(&self, path: &str) -> bool {
        Path::new(path).exists()
    }

    fn read(&self, path: &str) -> std::io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn is_symbolic_link(&self, path: &str) -> Option<bool> {
        Some(
            std::fs::symlink_metadata(path)
                .map(|metadata| metadata.file_type().is_symlink())
                .unwrap_or(true),
        )
    }

    fn realpath(&self, path: &str) -> Option<String> {
        std::fs::canonicalize(path)
            .ok()
            .map(|resolved| to_posix_path(&resolved.to_string_lossy()))
    }
}

#[derive(Default)]
pub struct LoadOmoConfigOptions<'a> {
    pub cwd: Option<String>,
    pub env: Option<OmoConfigEnv>,
    pub file_system: Option<&'a dyn OmoConfigReadFileSystem>,
    pub harness: Option<String>,
    pub platform: Option<String>,
    pub profile: Option<String>,
}
