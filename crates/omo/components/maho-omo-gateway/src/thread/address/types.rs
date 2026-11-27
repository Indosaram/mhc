//! Address resolver context, options, and outcomes (`thread/address/types.ts`).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::super::address_core::DeviceRef;
use super::super::addressing::{IdPrefixMode, ThreadAddressEntry};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

pub const UNKNOWN_CALLER: &str = "unknown-caller";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteDeviceInfo {
    pub device: String,
    pub label: String,
    pub online: bool,
    pub last_seen_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteSessionRow {
    pub device: String,
    pub durable_id: Option<String>,
    pub session_id: String,
    pub name: Option<String>,
    pub first_message: Option<String>,
    pub folder: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteListing {
    pub rows: Vec<RemoteSessionRow>,
    pub incomplete: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeAnswer {
    Found { key: String },
    None,
    Silent { reason: String },
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocateAnswer {
    Ok { session_id: String },
    Refused { reason: String },
}

/// Optional cross-device seam (`O02` supplies this; `O01` leaves `remote: None`).
pub trait RemoteAddressSurface: Send + Sync {
    fn here(&self) -> Option<String>;
    fn devices(&self) -> BoxFuture<Result<Vec<RemoteDeviceInfo>, String>>;
    fn list_sessions(&self, device: Option<&str>) -> BoxFuture<Result<RemoteListing, String>>;
    fn resolve_alias(&self, alias: &str) -> BoxFuture<Option<(String, String)>>;
    fn locate(&self, device: &str, session_id: &str) -> BoxFuture<LocateAnswer>;
    fn lookup(&self, device: &str, thread_id: &str) -> BoxFuture<ProbeAnswer>;
}

pub type LocalEntriesForFn = Arc<dyn Fn(&str) -> Vec<ThreadAddressEntry> + Send + Sync>;

#[derive(Clone)]
pub struct ResolveContext {
    pub remote: Option<Arc<dyn RemoteAddressSurface>>,
    pub caller_workspace_root: String,
    pub local: Vec<ThreadAddressEntry>,
    pub local_for: Option<LocalEntriesForFn>,
    pub caller_id: String,
}

impl ResolveContext {
    pub fn local_only(
        caller_id: impl Into<String>,
        caller_workspace_root: impl Into<String>,
        local: Vec<ThreadAddressEntry>,
    ) -> Self {
        Self {
            remote: None,
            caller_workspace_root: caller_workspace_root.into(),
            local,
            local_for: None,
            caller_id: caller_id.into(),
        }
    }

    pub fn local_entries_for(&self, text: &str) -> Vec<ThreadAddressEntry> {
        match &self.local_for {
            Some(f) => f(text),
            None => self.local.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveMatch {
    Exact,
    Fuzzy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveNeed {
    Address,
    Online,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvePin {
    pub device: String,
    pub thread: String,
}

#[derive(Debug, Clone, Default)]
pub struct ResolveOptions {
    pub all_scope: bool,
    pub match_mode: Option<ResolveMatch>,
    pub id_prefix: IdPrefixMode,
    pub probe: bool,
    pub pin: Option<ResolvePin>,
    pub unique_holder: bool,
    pub need: Option<ResolveNeed>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    SelfRef,
    Id,
    SessionId,
    Alias,
    Name,
    IdPrefix,
    Probe,
}

impl Resolution {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SelfRef => "self",
            Self::Id => "id",
            Self::SessionId => "session_id",
            Self::Alias => "alias",
            Self::Name => "name",
            Self::IdPrefix => "id_prefix",
            Self::Probe => "probe",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum ResolveWhere {
    Here { entry: ThreadAddressEntry },
    Device { row: RemoteSessionRow },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub thread: String,
    pub address: String,
    pub device: Option<DeviceRef>,
    pub where_located: ResolveWhere,
    pub entry: ThreadAddressEntry,
    pub resolution: Resolution,
    pub all_scope: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub entry: ThreadAddressEntry,
    pub resolution: Resolution,
    pub all_scope: Option<bool>,
}
