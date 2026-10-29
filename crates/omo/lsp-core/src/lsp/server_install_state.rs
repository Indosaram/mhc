//! Port of TS `server-install-state.ts`: persisted user install decisions.

use crate::request_context::LspRequestContextUnavailableError;
use crate::request_context::lsp_request_context;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallDecision {
    Declined,
    Allowed,
}

impl InstallDecision {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "declined" => Some(Self::Declined),
            "allowed" => Some(Self::Allowed),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Declined => "declined",
            Self::Allowed => "allowed",
        }
    }
}

/// TS `isInstallDecision`.
pub fn is_install_decision(value: &Value) -> bool {
    value.as_str().and_then(InstallDecision::parse).is_some()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallDecisionRecord {
    pub decision: InstallDecision,
    pub decided_at: String,
}

/// TS `getInstallDecisionsPath`.
pub fn get_install_decisions_path() -> Result<String, LspRequestContextUnavailableError> {
    lsp_request_context().map(|context| context.install_decisions_path)
}

/// TS `loadInstallDecisions`; invalid or unreadable files yield an empty set.
pub fn load_install_decisions()
-> Result<Vec<(String, InstallDecisionRecord)>, LspRequestContextUnavailableError> {
    let path = get_install_decisions_path()?;
    Ok(read_decisions(&path).unwrap_or_default())
}

fn read_decisions(path: &str) -> Option<Vec<(String, InstallDecisionRecord)>> {
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: Value = serde_json::from_str(&text).ok()?;
    let record = parsed.as_object()?;
    record
        .iter()
        .map(|(id, value)| {
            let entry = value.as_object()?;
            let decision = InstallDecision::parse(entry.get("decision")?.as_str()?)?;
            let decided_at = entry.get("decidedAt")?.as_str()?.to_string();
            Some((
                id.clone(),
                InstallDecisionRecord {
                    decision,
                    decided_at,
                },
            ))
        })
        .collect()
}

/// TS `loadInstallDecision`.
pub fn load_install_decision(
    server_id: &str,
) -> Result<Option<InstallDecisionRecord>, LspRequestContextUnavailableError> {
    Ok(load_install_decisions()?
        .into_iter()
        .find(|(id, _)| id == server_id)
        .map(|(_, record)| record))
}

/// TS `recordInstallDecision` (writes via tmp file + rename).
pub fn record_install_decision(
    server_id: &str,
    decision: InstallDecision,
    decided_at: Option<String>,
) -> Result<(), InstallDecisionWriteError> {
    let path = get_install_decisions_path()?;
    let mut decisions = read_decisions(&path).unwrap_or_default();
    let record = InstallDecisionRecord {
        decision,
        decided_at: decided_at.unwrap_or_else(now_iso),
    };
    match decisions.iter_mut().find(|(id, _)| id == server_id) {
        Some(slot) => slot.1 = record,
        None => decisions.push((server_id.to_string(), record)),
    }
    let mut out = Map::new();
    for (id, record) in decisions {
        out.insert(
            id,
            json!({ "decision": record.decision.as_str(), "decidedAt": record.decided_at }),
        );
    }
    if let Some(parent) = Path::new(&path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = format!("{path}.tmp");
    let body = serde_json::to_string_pretty(&Value::Object(out)).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, format!("{body}\n"))?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum InstallDecisionWriteError {
    #[error(transparent)]
    Context(#[from] LspRequestContextUnavailableError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// `new Date().toISOString()` (UTC, millisecond precision).
pub fn now_iso() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    iso_from_millis(i64::try_from(now.as_millis()).unwrap_or(i64::MAX))
}

fn iso_from_millis(millis: i64) -> String {
    let secs = millis.div_euclid(1000);
    let ms = millis.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}
