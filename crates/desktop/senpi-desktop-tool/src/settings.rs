//! Settings parsing and validation matching upstream `src/settings.ts`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::host_policy::{default_stop_hotkey, is_supported_host};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditLogSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for AuditLogSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotGcSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_stale_ms")]
    pub stale_ms: u64,
    #[serde(default = "default_scan_interval_ms")]
    pub scan_interval_ms: u64,
}

fn default_true() -> bool {
    true
}

fn default_stale_ms() -> u64 {
    43_200_000
}

fn default_scan_interval_ms() -> u64 {
    1_800_000
}

impl Default for ScreenshotGcSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            stale_ms: default_stale_ms(),
            scan_interval_ms: default_scan_interval_ms(),
        }
    }
}

/// Raw input matching the JSON schema of `ComputerSettingsSchema`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerSettingsInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_max_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_hotkey: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_host_relay_only_stop: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub macos_canary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit_log: Option<AuditLogSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_gc: Option<ScreenshotGcSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cua_adapter: Option<bool>,
}

/// Fully resolved `computer.*` settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerSettings {
    pub enabled: bool,
    pub display: String,
    pub max_width: u32,
    pub max_height: u32,
    pub screenshot_max_bytes: u64,
    pub stop_hotkey: String,
    pub allow_host_relay_only_stop: bool,
    pub macos_canary: String,
    pub audit_log: AuditLogSettings,
    pub screenshot_gc: ScreenshotGcSettings,
    pub engine_path: Option<String>,
    pub cua_adapter: bool,
}

/// A `computer` settings block that does not match `ComputerSettingsSchema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("invalid computer settings: {}", issues.join("; "))]
pub struct ComputerSettingsError {
    pub issues: Vec<String>,
}

impl ComputerSettingsError {
    pub fn new(issues: Vec<String>) -> Self {
        Self { issues }
    }
}

/// Parses the raw `computer` settings value and fills the per-host defaults.
pub fn resolve_computer_settings(
    raw: Option<&Value>,
    platform: &str,
) -> Result<ComputerSettings, ComputerSettingsError> {
    let value = match raw {
        None => Value::Object(Default::default()),
        Some(v) => {
            if !v.is_object() {
                return Err(ComputerSettingsError::new(vec![
                    "computer: expected object".to_string(),
                ]));
            }
            v.clone()
        }
    };

    let obj = match value.as_object() {
        Some(o) => o,
        None => {
            return Err(ComputerSettingsError::new(vec![
                "computer: expected object".to_string(),
            ]))
        }
    };

    let mut issues = Vec::new();

    // Check unknown keys
    for key in obj.keys() {
        match key.as_str() {
            "enabled" | "display" | "maxWidth" | "maxHeight" | "screenshotMaxBytes"
            | "stopHotkey" | "allowHostRelayOnlyStop" | "macosCanary" | "auditLog"
            | "screenshotGc" | "enginePath" | "cuaAdapter" => {}
            other => {
                issues.push(format!("computer: unknown property '{other}'"));
            }
        }
    }

    let enabled = match obj.get("enabled") {
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            issues.push("/enabled: must be boolean".to_string());
            is_supported_host(platform)
        }
        None => is_supported_host(platform),
    };

    let display = match obj.get("display") {
        Some(Value::String(s)) => s.clone(),
        Some(_) => {
            issues.push("/display: must be string".to_string());
            "all".to_string()
        }
        None => "all".to_string(),
    };

    let max_width = match obj.get("maxWidth") {
        Some(Value::Number(n)) if n.as_u64().is_some() && n.as_u64().unwrap_or(0) >= 1 => {
            n.as_u64().unwrap_or(3840) as u32
        }
        Some(_) => {
            issues.push("/maxWidth: must be integer >= 1".to_string());
            3840
        }
        None => 3840,
    };

    let max_height = match obj.get("maxHeight") {
        Some(Value::Number(n)) if n.as_u64().is_some() && n.as_u64().unwrap_or(0) >= 1 => {
            n.as_u64().unwrap_or(2400) as u32
        }
        Some(_) => {
            issues.push("/maxHeight: must be integer >= 1".to_string());
            2400
        }
        None => 2400,
    };

    let screenshot_max_bytes = match obj.get("screenshotMaxBytes") {
        Some(Value::Number(n)) if n.as_u64().is_some() && n.as_u64().unwrap_or(0) >= 1 => {
            n.as_u64().unwrap_or(5_000_000)
        }
        Some(_) => {
            issues.push("/screenshotMaxBytes: must be integer >= 1".to_string());
            5_000_000
        }
        None => 5_000_000,
    };

    let stop_hotkey = match obj.get("stopHotkey") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(_) => {
            issues.push("/stopHotkey: must be non-empty string".to_string());
            default_stop_hotkey(platform).to_string()
        }
        None => default_stop_hotkey(platform).to_string(),
    };

    let allow_host_relay_only_stop = match obj.get("allowHostRelayOnlyStop") {
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            issues.push("/allowHostRelayOnlyStop: must be boolean".to_string());
            false
        }
        None => false,
    };

    let macos_canary = match obj.get("macosCanary") {
        Some(Value::String(s)) if s == "session" || s == "off" => s.clone(),
        Some(_) => {
            issues.push("/macosCanary: must be 'session' or 'off'".to_string());
            "session".to_string()
        }
        None => "session".to_string(),
    };

    let audit_log = match obj.get("auditLog") {
        Some(Value::Object(o)) => {
            let mut al = AuditLogSettings::default();
            for k in o.keys() {
                if k != "enabled" {
                    issues.push(format!("/auditLog: unknown property '{k}'"));
                }
            }
            if let Some(e) = o.get("enabled") {
                if let Some(b) = e.as_bool() {
                    al.enabled = b;
                } else {
                    issues.push("/auditLog/enabled: must be boolean".to_string());
                }
            }
            al
        }
        Some(_) => {
            issues.push("/auditLog: must be object".to_string());
            AuditLogSettings::default()
        }
        None => AuditLogSettings::default(),
    };

    let screenshot_gc = match obj.get("screenshotGc") {
        Some(Value::Object(o)) => {
            let mut gc = ScreenshotGcSettings::default();
            for k in o.keys() {
                if k != "enabled" && k != "staleMs" && k != "scanIntervalMs" {
                    issues.push(format!("/screenshotGc: unknown property '{k}'"));
                }
            }
            if let Some(e) = o.get("enabled") {
                if let Some(b) = e.as_bool() {
                    gc.enabled = b;
                } else {
                    issues.push("/screenshotGc/enabled: must be boolean".to_string());
                }
            }
            if let Some(s) = o.get("staleMs") {
                if let Some(n) = s.as_u64() {
                    gc.stale_ms = n;
                } else {
                    issues.push("/screenshotGc/staleMs: must be integer >= 0".to_string());
                }
            }
            if let Some(s) = o.get("scanIntervalMs") {
                if let Some(n) = s.as_u64() {
                    gc.scan_interval_ms = n;
                } else {
                    issues.push("/screenshotGc/scanIntervalMs: must be integer >= 0".to_string());
                }
            }
            gc
        }
        Some(_) => {
            issues.push("/screenshotGc: must be object".to_string());
            ScreenshotGcSettings::default()
        }
        None => ScreenshotGcSettings::default(),
    };

    let engine_path = match obj.get("enginePath") {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(_) => {
            issues.push("/enginePath: must be non-empty string".to_string());
            None
        }
        None => None,
    };

    let cua_adapter = match obj.get("cuaAdapter") {
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            issues.push("/cuaAdapter: must be boolean".to_string());
            false
        }
        None => false,
    };

    if !issues.is_empty() {
        return Err(ComputerSettingsError::new(issues));
    }

    Ok(ComputerSettings {
        enabled,
        display,
        max_width,
        max_height,
        screenshot_max_bytes,
        stop_hotkey,
        allow_host_relay_only_stop,
        macos_canary,
        audit_log,
        screenshot_gc,
        engine_path,
        cua_adapter,
    })
}
