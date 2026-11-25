//! Address resolution refusal data (`packages/thread-address-core/src/refusals.ts`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCode {
    InvalidArguments,
    CallerContextMissing,
    NotFound,
    AmbiguousTarget,
    DeviceOffline,
    HostUnavailable,
    ScopeDenied,
}

impl RefusalCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArguments => "invalid_arguments",
            Self::CallerContextMissing => "caller_context_missing",
            Self::NotFound => "not_found",
            Self::AmbiguousTarget => "ambiguous_target",
            Self::DeviceOffline => "device_offline",
            Self::HostUnavailable => "host_unavailable",
            Self::ScopeDenied => "scope_denied",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCandidate {
    pub device: String,
    pub name: String,
    pub online: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotAskedReason {
    RemoteUnavailable,
    WakeUnavailable,
    LookupUnavailable,
}

impl NotAskedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RemoteUnavailable => "remote_unavailable",
            Self::WakeUnavailable => "wake_unavailable",
            Self::LookupUnavailable => "lookup_unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRow {
    pub id: String,
    pub name: String,
    pub online: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareBy {
    DevicesShareName,
    DevicesSharePrefix,
}

impl ShareBy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DevicesShareName => "devices_share_name",
            Self::DevicesSharePrefix => "devices_share_prefix",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SilentReason {
    ListingIncomplete,
    DeviceUnanswered,
}

impl SilentReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ListingIncomplete => "listing_incomplete",
            Self::DeviceUnanswered => "device_unanswered",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddressRefusal {
    NoDevice {
        typed: String,
        candidates: Vec<DeviceCandidate>,
    },
    DevicesShare {
        by: ShareBy,
        typed: String,
        candidates: Vec<DeviceCandidate>,
    },
    DeviceOffline {
        typed: String,
        device: String,
        device_name: String,
        last_seen: Option<i64>,
        candidates: Vec<DeviceCandidate>,
    },
    DevicesSilent {
        reason: SilentReason,
        devices: Vec<String>,
        device_names: Vec<String>,
        device_reasons: BTreeMap<String, String>,
    },
    DeviceNotAsked {
        reason: NotAskedReason,
        thread: String,
        device: String,
        device_name: String,
    },
    NoThreadOnDevice {
        thread: String,
        device: String,
        device_name: String,
    },
}

const CANDIDATE_LIMIT: usize = 10;

fn device_candidates(rows: &[DeviceRow]) -> Vec<DeviceCandidate> {
    rows.iter()
        .take(CANDIDATE_LIMIT)
        .map(|row| DeviceCandidate {
            device: row.id.clone(),
            name: row.name.clone(),
            online: row.online,
        })
        .collect()
}

/// A device as every message names it: `name (id)`, or the id alone when its name is empty or the id itself.
pub fn device_label(id: &str, name: &str) -> String {
    if name.is_empty() || name == id {
        id.to_owned()
    } else {
        format!("{name} ({id})")
    }
}

pub fn no_device(typed: &str, devices: &[DeviceRow]) -> AddressRefusal {
    AddressRefusal::NoDevice {
        typed: typed.to_owned(),
        candidates: device_candidates(devices),
    }
}

pub fn devices_share(by: ShareBy, typed: &str, matches: &[DeviceRow]) -> AddressRefusal {
    AddressRefusal::DevicesShare {
        by,
        typed: typed.to_owned(),
        candidates: device_candidates(matches),
    }
}

pub fn device_offline(typed: &str, device: &DeviceRow, matched: &[DeviceRow]) -> AddressRefusal {
    AddressRefusal::DeviceOffline {
        typed: typed.to_owned(),
        device: device.id.clone(),
        device_name: device.name.clone(),
        last_seen: device.last_seen_ms,
        candidates: device_candidates(matched),
    }
}

pub fn devices_silent(
    reason: SilentReason,
    devices: &[(&str, &str, Option<&str>)],
) -> AddressRefusal {
    let mut device_reasons = BTreeMap::new();
    for (id, _, r) in devices {
        if let Some(reason_text) = r {
            device_reasons.insert((*id).to_owned(), (*reason_text).to_owned());
        }
    }
    AddressRefusal::DevicesSilent {
        reason,
        devices: devices.iter().map(|(id, _, _)| (*id).to_owned()).collect(),
        device_names: devices
            .iter()
            .map(|(_, name, _)| (*name).to_owned())
            .collect(),
        device_reasons,
    }
}

pub fn device_not_asked(
    reason: NotAskedReason,
    thread: &str,
    device_id: &str,
    device_name: &str,
) -> AddressRefusal {
    AddressRefusal::DeviceNotAsked {
        reason,
        thread: thread.to_owned(),
        device: device_id.to_owned(),
        device_name: device_name.to_owned(),
    }
}

pub fn no_thread_on_device(thread: &str, device_id: &str, device_name: &str) -> AddressRefusal {
    AddressRefusal::NoThreadOnDevice {
        thread: thread.to_owned(),
        device: device_id.to_owned(),
        device_name: device_name.to_owned(),
    }
}
