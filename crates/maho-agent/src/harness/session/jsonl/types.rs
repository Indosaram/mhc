//! Port of senpi packages/agent/src/harness/session/jsonl/types.ts.

use crate::harness::session::types::{SessionCreateOptions, SessionMetadata};
use crate::harness::types::FileSystem;

pub const JSONL_FORMAT_VERSION: u32 = 4;
pub const JSONL_STORAGE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonlStorageHeader {
    pub v: u32,
    pub kind: String,
    pub id: String,
    pub storage_version: u32,
    pub created_at: i64,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_parent_session_path: Option<String>,
    /// Sequence high-water mark written by snapshot rewrites.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_seq: Option<i64>,
}

impl JsonlStorageHeader {
    pub fn new(id: impl Into<String>, storage_version: u32, created_at: i64, cwd: impl Into<String>) -> Self {
        Self {
            v: JSONL_FORMAT_VERSION,
            kind: "header".to_owned(),
            id: id.into(),
            storage_version,
            created_at,
            cwd: cwd.into(),
            parent_session_id: None,
            legacy_parent_session_path: None,
            next_seq: None,
        }
    }
}

#[derive(Clone)]
pub struct JsonlStorageOptions {
    pub file_system: std::sync::Arc<dyn FileSystem>,
    pub path: String,
    pub now: Option<std::sync::Arc<dyn Fn() -> i64 + Send + Sync>>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonlSessionMetadata {
    pub id: String,
    pub created_at: i64,
    pub storage_version: u32,
    pub cwd: String,
    pub path: String,
    /// Filesystem modification time as milliseconds since Unix epoch.
    pub modified_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_parent_session_path: Option<String>,
}

impl JsonlSessionMetadata {
    pub fn to_session_metadata(&self) -> SessionMetadata {
        SessionMetadata {
            id: self.id.clone(),
            created_at: self.created_at,
            storage_version: self.storage_version,
            cwd: Some(self.cwd.clone()),
            parent_session_id: self.parent_session_id.clone(),
            legacy_parent_session_path: self.legacy_parent_session_path.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonlSessionCreateOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    pub cwd: String,
}

impl JsonlSessionCreateOptions {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self {
            id: None,
            parent_session_id: None,
            cwd: cwd.into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonlSessionListOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Clone)]
pub struct JsonlSessionRepoOptions {
    pub file_system: std::sync::Arc<dyn FileSystem>,
    pub sessions_root: String,
    pub now: Option<std::sync::Arc<dyn Fn() -> i64 + Send + Sync>>,
}

pub type SessionCreateOptionsAlias = SessionCreateOptions;
