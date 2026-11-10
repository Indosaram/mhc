//! Parsing lock records: the JSON each lock file holds about its holder.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-lock-record.md`. The record is how a waiter decides
//! whether the process holding a lock is still alive. A record that parses when it should not — a
//! zero pid, a non-string hostname, an empty purpose — makes the store take a dead lock for a live one
//! (or worse, steal a live lock), so every field is validated rather than defaulted.
use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockRecord {
    pub pid: u32,
    /// Opaque process-start token, used to tell a reused pid from the original holder.
    pub process_start: String,
    pub hostname: String,
    /// Unique per acquisition: two records from one process differ.
    pub nonce: String,
    pub created_at: String,
    pub purpose: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_activity_lock: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CreateLockRecordOptions {
    pub run_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockRecordError {
    EmptyPurpose,
}

impl fmt::Display for LockRecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPurpose => f.write_str("lock purpose must not be empty"),
        }
    }
}

impl std::error::Error for LockRecordError {}

/// Parse a lock record, rejecting anything that would let a waiter misjudge liveness.
///
/// Returns `None` rather than an error: every caller's reaction to a malformed record is the same
/// (treat the lock as unusable), so a typed error would only be discarded at every call site.
pub fn parse_lock_record(content: &str) -> Option<LockRecord> {
    let value: serde_json::Value = serde_json::from_str(content).ok()?;
    let object = value.as_object()?;

    let pid = object.get("pid")?.as_i64()?;
    let pid = u32::try_from(pid).ok().filter(|pid| *pid != 0)?;

    let process_start = non_empty_str(object.get("process_start")?)?;
    let hostname = non_empty_str(object.get("hostname")?)?;
    let nonce = non_empty_str(object.get("nonce")?)?;
    let created_at = non_empty_str(object.get("created_at")?)?;
    if chrono::DateTime::parse_from_rfc3339(&created_at).is_err() {
        return None;
    }
    let purpose = non_empty_str(object.get("purpose")?)?;

    let run_id = match object.get("run_id") {
        None => None,
        Some(serde_json::Value::String(text)) if !text.is_empty() => Some(text.clone()),
        Some(_) => return None,
    };

    Some(LockRecord {
        pid,
        process_start,
        hostname,
        nonce,
        created_at,
        purpose,
        run_id,
        git_activity_lock: matches!(
            object.get("git_activity_lock"),
            Some(serde_json::Value::Bool(true))
        )
        .then_some(true),
    })
}

fn non_empty_str(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// What the record needs from the host. Injected so record creation is testable without reading the
/// real clock, hostname or process table.
pub trait LockHost {
    fn process_id(&self) -> u32;
    fn process_start(&self, pid: u32) -> Option<String>;
    fn hostname(&self) -> String;
    fn nonce(&self) -> String;
    fn now(&self) -> String;
    fn os_file_lock_available(&self) -> bool;
}

pub fn create_lock_record(
    host: &dyn LockHost,
    purpose: &str,
    options: CreateLockRecordOptions,
) -> Result<LockRecord, LockRecordError> {
    if purpose.is_empty() {
        return Err(LockRecordError::EmptyPurpose);
    }
    let pid = host.process_id();
    Ok(LockRecord {
        pid,
        process_start: host
            .process_start(pid)
            .unwrap_or_else(|| "unavailable".to_owned()),
        hostname: host.hostname(),
        nonce: host.nonce(),
        created_at: host.now(),
        purpose: purpose.to_owned(),
        run_id: options.run_id,
        git_activity_lock: host.os_file_lock_available().then_some(true),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_json() -> String {
        r#"{
            "pid": 12345,
            "process_start": "ps-lstart:Sun Sep 27 00:00:00 2026",
            "hostname": "test-host",
            "nonce": "test-nonce-1234",
            "created_at": "2026-09-28T12:00:00.000Z",
            "purpose": "reflection-scheduler",
            "run_id": "run-abc",
            "git_activity_lock": true
        }"#
        .to_owned()
    }

    struct FakeHost {
        process_start: Option<String>,
        os_lock: bool,
    }

    impl LockHost for FakeHost {
        fn process_id(&self) -> u32 {
            4321
        }
        fn process_start(&self, _pid: u32) -> Option<String> {
            self.process_start.clone()
        }
        fn hostname(&self) -> String {
            "unit-host".to_owned()
        }
        fn nonce(&self) -> String {
            "unit-nonce".to_owned()
        }
        fn now(&self) -> String {
            "2026-11-10T00:00:00.000Z".to_owned()
        }
        fn os_file_lock_available(&self) -> bool {
            self.os_lock
        }
    }

    fn host() -> FakeHost {
        FakeHost {
            process_start: Some("ps-lstart:token".to_owned()),
            os_lock: true,
        }
    }

    #[test]
    fn a_valid_record_round_trips() {
        let original = LockRecord {
            pid: 12345,
            process_start: "ps-lstart:Sun Sep 27 00:00:00 2026".to_owned(),
            hostname: "test-host".to_owned(),
            nonce: "test-nonce-1234".to_owned(),
            created_at: "2026-09-28T12:00:00.000Z".to_owned(),
            purpose: "reflection-scheduler".to_owned(),
            run_id: Some("run-abc".to_owned()),
            git_activity_lock: Some(true),
        };
        let parsed =
            parse_lock_record(&serde_json::to_string(&original).expect("json")).expect("record");
        assert_eq!(parsed, original);
    }

    #[test]
    fn the_optional_fields_may_be_absent() {
        let json = r#"{
            "pid": 1,
            "process_start": "a",
            "hostname": "h",
            "nonce": "n",
            "created_at": "2026-09-28T12:00:00Z",
            "purpose": "p"
        }"#;
        let record = parse_lock_record(json).expect("record");
        assert_eq!(record.run_id, None);
        assert_eq!(record.git_activity_lock, None);
    }

    #[test]
    fn a_record_without_a_pid_is_rejected() {
        assert!(parse_lock_record("{}").is_none());
        assert!(parse_lock_record(r#"{"pid": 0}"#).is_none());
        assert!(parse_lock_record(r#"{"pid": -10}"#).is_none());
    }

    #[test]
    fn a_pid_above_the_u32_range_is_rejected() {
        let json = valid_json().replace("12345", "4294967296");
        assert!(parse_lock_record(&json).is_none());
    }

    #[test]
    fn a_pid_of_u32_max_is_accepted() {
        let json = valid_json().replace("12345", "4294967295");
        assert_eq!(parse_lock_record(&json).expect("record").pid, u32::MAX);
    }

    #[test]
    fn non_json_that_is_not_an_object_is_rejected() {
        assert!(parse_lock_record("not-json").is_none());
        assert!(parse_lock_record("[]").is_none());
        assert!(parse_lock_record("null").is_none());
        assert!(parse_lock_record("\"text\"").is_none());
    }

    #[test]
    fn a_missing_required_field_is_rejected() {
        for field in [
            "process_start",
            "hostname",
            "nonce",
            "created_at",
            "purpose",
        ] {
            let mut object: serde_json::Value =
                serde_json::from_str(&valid_json()).expect("fixture is json");
            object.as_object_mut().expect("object").remove(field);
            assert!(
                parse_lock_record(&object.to_string()).is_none(),
                "missing {field} must reject"
            );
        }
    }

    #[test]
    fn a_non_string_required_field_is_rejected() {
        for field in [
            "process_start",
            "hostname",
            "nonce",
            "created_at",
            "purpose",
        ] {
            let mut object: serde_json::Value =
                serde_json::from_str(&valid_json()).expect("fixture is json");
            object
                .as_object_mut()
                .expect("object")
                .insert(field.to_owned(), serde_json::Value::Number(1.into()));
            assert!(
                parse_lock_record(&object.to_string()).is_none(),
                "non-string {field} must reject"
            );
        }
    }

    #[test]
    fn an_empty_hostname_is_rejected() {
        let json = valid_json().replace("\"test-host\"", "\"\"");
        assert!(parse_lock_record(&json).is_none());
    }

    #[test]
    fn an_empty_nonce_is_rejected() {
        let json = valid_json().replace("\"test-nonce-1234\"", "\"\"");
        assert!(parse_lock_record(&json).is_none());
    }

    #[test]
    fn an_empty_process_start_is_rejected() {
        let json = valid_json().replace("\"ps-lstart:Sun Sep 27 00:00:00 2026\"", "\"\"");
        assert!(parse_lock_record(&json).is_none());
    }

    #[test]
    fn an_empty_purpose_is_rejected() {
        let json = valid_json().replace("\"reflection-scheduler\"", "\"\"");
        assert!(parse_lock_record(&json).is_none());
    }

    #[test]
    fn a_created_at_that_is_not_rfc3339_is_rejected() {
        for bad in ["yesterday", "2026-09-28", "2026-13-45T99:99:99Z", ""] {
            let json = valid_json().replace("2026-09-28T12:00:00.000Z", bad);
            assert!(
                parse_lock_record(&json).is_none(),
                "expected rejection for {bad:?}"
            );
        }
    }

    #[test]
    fn a_null_run_id_is_rejected_rather_than_treated_as_absent() {
        let json = valid_json().replace("\"run_id\": \"run-abc\"", "\"run_id\": null");
        assert!(parse_lock_record(&json).is_none());
    }

    #[test]
    fn an_empty_run_id_is_rejected() {
        let json = valid_json().replace("\"run_id\": \"run-abc\"", "\"run_id\": \"\"");
        assert!(parse_lock_record(&json).is_none());
    }

    #[test]
    fn a_non_string_run_id_is_rejected() {
        let json = valid_json().replace("\"run_id\": \"run-abc\"", "\"run_id\": 7");
        assert!(parse_lock_record(&json).is_none());
    }

    #[test]
    fn git_activity_lock_is_true_only_when_literally_true() {
        let json = valid_json().replace(
            "\"git_activity_lock\": true",
            "\"git_activity_lock\": false",
        );
        assert_eq!(
            parse_lock_record(&json).expect("record").git_activity_lock,
            None
        );
        let json = valid_json().replace(
            "\"git_activity_lock\": true",
            "\"git_activity_lock\": \"true\"",
        );
        assert_eq!(
            parse_lock_record(&json).expect("record").git_activity_lock,
            None
        );
    }

    #[test]
    fn a_korean_hostname_survives() {
        let json = valid_json().replace("\"test-host\"", "\"한국어-호스트\"");
        assert_eq!(
            parse_lock_record(&json).expect("record").hostname,
            "한국어-호스트"
        );
    }

    #[test]
    fn an_empty_purpose_cannot_be_created() {
        assert_eq!(
            create_lock_record(&host(), "", CreateLockRecordOptions::default()),
            Err(LockRecordError::EmptyPurpose)
        );
    }

    #[test]
    fn a_created_record_carries_the_host_identity() {
        let record = create_lock_record(
            &host(),
            "memory-write",
            CreateLockRecordOptions {
                run_id: Some("run-42".to_owned()),
            },
        )
        .expect("record");
        assert_eq!(record.pid, 4321);
        assert_eq!(record.process_start, "ps-lstart:token");
        assert_eq!(record.hostname, "unit-host");
        assert_eq!(record.nonce, "unit-nonce");
        assert_eq!(record.created_at, "2026-11-10T00:00:00.000Z");
        assert_eq!(record.purpose, "memory-write");
        assert_eq!(record.run_id.as_deref(), Some("run-42"));
        assert_eq!(record.git_activity_lock, Some(true));
    }

    #[test]
    fn an_unavailable_process_start_is_recorded_as_unavailable() {
        let host = FakeHost {
            process_start: None,
            os_lock: false,
        };
        let record = create_lock_record(&host, "notice", CreateLockRecordOptions::default())
            .expect("record");
        assert_eq!(record.process_start, "unavailable");
        assert_eq!(record.git_activity_lock, None);
    }

    #[test]
    fn a_created_record_parses_back() {
        let record = create_lock_record(&host(), "receipts", CreateLockRecordOptions::default())
            .expect("record");
        let json = serde_json::to_string(&record).expect("json");
        assert_eq!(parse_lock_record(&json).expect("parsed"), record);
    }

    #[test]
    fn a_created_record_without_optional_fields_omits_them_from_json() {
        let host = FakeHost {
            process_start: Some("ps-lstart:token".to_owned()),
            os_lock: false,
        };
        let record = create_lock_record(&host, "notice", CreateLockRecordOptions::default())
            .expect("record");
        let json = serde_json::to_string(&record).expect("json");
        assert!(!json.contains("run_id"));
        assert!(!json.contains("git_activity_lock"));
    }
}
