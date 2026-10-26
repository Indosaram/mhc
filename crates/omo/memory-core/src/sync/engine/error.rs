//! The engine's one error type: every port and journal failure funnels through it.

use super::adapters::git_sync_repo::SyncCommitError;
use super::adapters::http_transport::SyncHttpError;
use super::batches::OversizedItemError;
use super::engine_deps::CrashInjected;
use crate::journal::JournalError;
use crate::sync::item::ItemError;

#[derive(Debug)]
pub enum EngineError {
    Account(super::super::account::http::AccountHttpError),
    Recovery(super::super::recovery::RecoveryError),
    Seal(super::super::account::sealer::SealRefusedError),
    Auth,
    Network,
    StrandedLock(Vec<String>),
    Io(std::io::Error),
    Json(serde_json::Error),
    Item(ItemError),
    Commit(SyncCommitError),
    Http(SyncHttpError),
    Crash(CrashInjected),
    Oversized(OversizedItemError),
    Lock(String),
    Other(String),
}

pub type EngineResult<T> = Result<T, EngineError>;

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Account(e) => e.fmt(f),
            Self::Recovery(e) => e.fmt(f),
            Self::Seal(e) => e.fmt(f),
            Self::Auth => f.write_str("sync authentication failed"),
            Self::Network => f.write_str("sync network request failed"),
            Self::StrandedLock(paths) => write!(f, "stranded git locks: {}", paths.join(", ")),
            Self::Io(e) => e.fmt(f),
            Self::Json(e) => e.fmt(f),
            Self::Item(e) => e.fmt(f),
            Self::Commit(e) => e.fmt(f),
            Self::Http(e) => e.fmt(f),
            Self::Crash(e) => e.fmt(f),
            Self::Oversized(e) => e.fmt(f),
            Self::Lock(s) | Self::Other(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<std::io::Error> for EngineError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<serde_json::Error> for EngineError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
impl From<ItemError> for EngineError {
    fn from(e: ItemError) -> Self {
        Self::Item(e)
    }
}
impl From<SyncCommitError> for EngineError {
    fn from(e: SyncCommitError) -> Self {
        Self::Commit(e)
    }
}
impl From<SyncHttpError> for EngineError {
    fn from(e: SyncHttpError) -> Self {
        Self::Http(e)
    }
}
impl From<CrashInjected> for EngineError {
    fn from(e: CrashInjected) -> Self {
        Self::Crash(e)
    }
}
impl From<OversizedItemError> for EngineError {
    fn from(e: OversizedItemError) -> Self {
        Self::Oversized(e)
    }
}
impl From<JournalError> for EngineError {
    fn from(e: JournalError) -> Self {
        match e {
            JournalError::Io(io) => Self::Io(io),
            JournalError::Json(json) => Self::Json(json),
            other => Self::Other(other.to_string()),
        }
    }
}
