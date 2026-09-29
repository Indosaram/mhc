//! Serializable lock records describing ownership, origin, and lifetime.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::process_identity::get_process_start_identity;

/// Serializable snapshot of a process holding an exclusive lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockRecord {
    pub pid: u32,
    pub process_start: String,
    pub hostname: String,
    pub nonce: String,
    pub created_at: String,
    pub purpose: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

/// Optional configuration for creating a lock record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreateLockRecordOptions {
    pub run_id: Option<String>,
}

/// Errors raised when creating lock records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockRecordError {
    EmptyPurpose,
}

impl fmt::Display for LockRecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPurpose => write!(f, "lock purpose must not be empty"),
        }
    }
}

impl std::error::Error for LockRecordError {}

/// Parses and validates a lock record from its JSON-serialized representation.
pub fn parse_lock_record(content: &str) -> Option<LockRecord> {
    let value: serde_json::Value = serde_json::from_str(content).ok()?;
    let obj = value.as_object()?;

    let pid_val = obj.get("pid")?;
    let pid_i64 = pid_val.as_i64()?;
    if pid_i64 <= 0 || pid_i64 > u32::MAX as i64 {
        return None;
    }
    let pid = pid_i64 as u32;

    let process_start = obj.get("process_start")?.as_str()?;
    if process_start.is_empty() {
        return None;
    }

    let hostname = obj.get("hostname")?.as_str()?;
    if hostname.is_empty() {
        return None;
    }

    let nonce = obj.get("nonce")?.as_str()?;
    if nonce.is_empty() {
        return None;
    }

    let created_at = obj.get("created_at")?.as_str()?;
    if created_at.is_empty() || crate::support::time::parse_rfc3339(created_at).is_none() {
        return None;
    }

    let purpose = obj.get("purpose")?.as_str()?;
    if purpose.is_empty() {
        return None;
    }

    let run_id = match obj.get("run_id") {
        Some(serde_json::Value::Null) => return None,
        Some(serde_json::Value::String(s)) => {
            if s.is_empty() {
                return None;
            }
            Some(s.clone())
        }
        Some(_) => return None,
        None => None,
    };

    Some(LockRecord {
        pid,
        process_start: process_start.to_string(),
        hostname: hostname.to_string(),
        nonce: nonce.to_string(),
        created_at: created_at.to_string(),
        purpose: purpose.to_string(),
        run_id,
    })
}

/// Creates a new lock record for the current process.
pub fn create_lock_record(
    purpose: &str,
    options: CreateLockRecordOptions,
) -> Result<LockRecord, LockRecordError> {
    if purpose.is_empty() {
        return Err(LockRecordError::EmptyPurpose);
    }

    let pid = std::process::id();
    let process_start =
        get_process_start_identity(pid).unwrap_or_else(|| "unavailable".to_string());
    let hostname = crate::support::host::hostname();
    let nonce = crate::support::random::random_uuid();
    let created_at = crate::support::time::now_iso();

    Ok(LockRecord {
        pid,
        process_start,
        hostname,
        nonce,
        created_at,
        purpose: purpose.to_string(),
        run_id: options.run_id,
    })
}

#[cfg(test)]
#[path = "lock_record_tests.rs"]
mod tests;
