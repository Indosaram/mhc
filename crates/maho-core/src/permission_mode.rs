//! Port of senpi packages/coding-agent/src/core/permission-mode.ts.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::permission_decision::{DecisionSession, PermissionPresetName, bind_decision_preset};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPresetSource {
    Client,
    HostDefault,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPresetStatus {
    Applied,
    Invalid,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionMode {
    pub permission_preset: String,
    pub permission_preset_source: PermissionPresetSource,
    pub permission_preset_status: PermissionPresetStatus,
    pub effective_permission_preset: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionModeReply {
    pub permission_preset: String,
    pub permission_preset_source: PermissionPresetSource,
    pub permission_preset_status: PermissionPresetStatus,
}

pub const PERMISSION_PRESET_FLAG: &str = "permission-preset";
pub const PERMISSION_PRESET_CHANGED_CHANNEL: &str = "permission_preset_changed";
pub const PERMISSION_MODE_ENTRY: &str = "permission_mode";

static CLIENT_PRESETS: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Default::default);
static HOST_DEFAULT_FLAG_SESSIONS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(Default::default);

pub fn mark_host_default_preset_flag(session_id: &str, is_host_default: bool) {
    let mut set = HOST_DEFAULT_FLAG_SESSIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if is_host_default {
        set.insert(session_id.to_owned());
    } else {
        set.remove(session_id);
    }
}

pub fn mark_client_permission_preset(session_id: &str, preset: Option<&str>) {
    let mut map = CLIENT_PRESETS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match preset {
        Some(p) => {
            map.insert(session_id.to_owned(), p.to_owned());
        }
        None => {
            map.remove(session_id);
        }
    }
}

pub fn client_preset_for_session(session_id: &str) -> Option<String> {
    CLIENT_PRESETS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(session_id)
        .cloned()
}

pub fn is_host_default_flag_session(session_id: &str) -> bool {
    HOST_DEFAULT_FLAG_SESSIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(session_id)
}

pub fn permission_mode_reply(mode: &PermissionMode) -> PermissionModeReply {
    PermissionModeReply {
        permission_preset: mode.permission_preset.clone(),
        permission_preset_source: mode.permission_preset_source,
        permission_preset_status: mode.permission_preset_status,
    }
}

pub fn persisted_client_preset(entries: &[Value]) -> Option<String> {
    for entry in entries.iter().rev() {
        if entry.get("type").and_then(Value::as_str) != Some("custom")
            || entry.get("customType").and_then(Value::as_str) != Some(PERMISSION_MODE_ENTRY)
        {
            continue;
        }
        let preset = entry.pointer("/data/preset").and_then(Value::as_str)?;
        return PermissionPresetName::parse(preset).map(|_| preset.to_owned());
    }
    None
}

pub fn invalid_mode(preset: &str) -> PermissionMode {
    PermissionMode {
        permission_preset: preset.to_owned(),
        permission_preset_source: PermissionPresetSource::HostDefault,
        permission_preset_status: PermissionPresetStatus::Invalid,
        effective_permission_preset: None,
    }
}

pub fn client_mode(
    preset: PermissionPresetName,
    enforced: Option<PermissionPresetName>,
) -> PermissionMode {
    PermissionMode {
        permission_preset: preset.as_str().to_owned(),
        permission_preset_source: PermissionPresetSource::Client,
        permission_preset_status: PermissionPresetStatus::Applied,
        effective_permission_preset: Some(enforced.unwrap_or(preset).as_str().to_owned()),
    }
}

pub fn host_default_mode(
    preset: PermissionPresetName,
    enforced: Option<PermissionPresetName>,
) -> PermissionMode {
    PermissionMode {
        permission_preset: preset.as_str().to_owned(),
        permission_preset_source: PermissionPresetSource::HostDefault,
        permission_preset_status: PermissionPresetStatus::Applied,
        effective_permission_preset: Some(enforced.unwrap_or(preset).as_str().to_owned()),
    }
}

pub fn bind_session_permission_decision(
    session_id: String,
    parent_session_id: Option<String>,
    flag_is_set: std::sync::Arc<dyn Fn() -> bool + Send + Sync>,
    read_entries: std::sync::Arc<dyn Fn() -> Vec<Value> + Send + Sync>,
) {
    let sid_for_named = session_id.clone();
    let sid_for_flag = session_id.clone();
    bind_decision_preset(DecisionSession {
        session_id,
        parent_session_id,
        named_preset: std::sync::Arc::new(move || client_preset_for_session(&sid_for_named)),
        launch_flag_named: std::sync::Arc::new(move || {
            !is_host_default_flag_session(&sid_for_flag) && flag_is_set()
        }),
        recorded_preset: std::sync::Arc::new(move || persisted_client_preset(&read_entries())),
    });
}
