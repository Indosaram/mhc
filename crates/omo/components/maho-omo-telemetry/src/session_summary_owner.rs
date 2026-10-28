//! Port of `packages/omo-senpi/src/components/telemetry/session-summary-owner.ts`.
//!
//! `owners/<pid>.json` holds the boot id, the process start time and a heartbeat of the OmO process that
//! wrote it. The start time recorded is the WRITING process's, because only the owner itself writes its
//! entry. Telling a live owner from a dead one is recovery's job (`session_summary_liveness`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use serde_json::{Value, json};

use crate::session_summary_files::write_json_atomic;
use crate::session_summary_process_start::read_process_start_time;

/// Random per loaded process, so a reused pid never passes for the process that wrote a file.
fn process_boot_id() -> &'static str {
    static BOOT_ID: OnceLock<String> = OnceLock::new();
    BOOT_ID.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOwner {
    pub pid: u32,
    pub boot_id: String,
}

pub fn current_process_owner() -> ProcessOwner {
    ProcessOwner {
        pid: std::process::id(),
        boot_id: process_boot_id().to_string(),
    }
}

pub type ProcessStartProbe = Arc<dyn Fn(u32) -> Option<String> + Send + Sync>;
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

#[derive(Clone)]
pub struct OwnerRegistryInput {
    pub dir: PathBuf,
    pub owner: ProcessOwner,
    /// Start-time probe; defaults to the real one. Injected by tests that simulate pid reuse.
    pub process_start_time: Option<ProcessStartProbe>,
    pub now: Option<Clock>,
}

pub struct OwnerRegistry {
    input: OwnerRegistryInput,
    own_start_time: Mutex<Option<Option<String>>>,
}

impl OwnerRegistry {
    pub fn new(input: OwnerRegistryInput) -> Self {
        Self {
            input,
            own_start_time: Mutex::new(None),
        }
    }

    pub fn register(&self) {
        let mut own = self
            .own_start_time
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if own.is_none() {
            let probe = self
                .input
                .process_start_time
                .clone()
                .unwrap_or_else(|| Arc::new(read_process_start_time));
            *own = Some(probe(std::process::id()));
        }
        let start_time = own.clone().flatten();
        drop(own);
        let now = self.input.now.clone().unwrap_or_else(|| {
            Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64()
                    * 1000.0
            })
        });
        let entry = json!({
            "pid": self.input.owner.pid,
            "bootId": self.input.owner.boot_id,
            "startTime": start_time,
            "heartbeatAt": now(),
        });
        let _ignored = write_json_atomic(
            &owner_entry_path(&self.input.dir, self.input.owner.pid),
            &entry,
            &self.input.owner,
        );
    }
}

pub fn owner_entry_path(dir: &Path, pid: u32) -> PathBuf {
    dir.join("owners").join(format!("{pid}.json"))
}

pub fn parse_owner(value: &Value) -> Option<ProcessOwner> {
    let pid = value.get("pid").and_then(Value::as_u64)?;
    let boot_id = value.get("bootId").and_then(Value::as_str)?;
    (u32::try_from(pid).is_ok() && !boot_id.is_empty()).then(|| ProcessOwner {
        pid: u32::try_from(pid).unwrap_or_default(),
        boot_id: boot_id.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(pid: u32, boot_id: &str) -> ProcessOwner {
        ProcessOwner {
            pid,
            boot_id: boot_id.into(),
        }
    }

    #[test]
    fn register_writes_boot_id_start_time_and_heartbeat() {
        let dir = tempfile::tempdir().unwrap();
        let registry = OwnerRegistry::new(OwnerRegistryInput {
            dir: dir.path().to_path_buf(),
            owner: owner(1234, "boot-1"),
            process_start_time: Some(Arc::new(|pid| Some(format!("proc:{pid}*9")))),
            now: Some(Arc::new(|| 5000.0)),
        });
        registry.register();
        let entry: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("owners/1234.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(entry["pid"], 1234);
        assert_eq!(entry["bootId"], "boot-1");
        assert_eq!(
            entry["startTime"],
            format!("proc:{}*9", std::process::id())
        );
        assert_eq!(entry["heartbeatAt"], 5000.0);
    }

    #[test]
    fn start_time_is_probed_once_across_refreshes() {
        let dir = tempfile::tempdir().unwrap();
        let probes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&probes);
        let ticks = Arc::new(Mutex::new(0.0));
        let clock = Arc::clone(&ticks);
        let registry = OwnerRegistry::new(OwnerRegistryInput {
            dir: dir.path().to_path_buf(),
            owner: owner(7, "boot-2"),
            process_start_time: Some(Arc::new(move |_| {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Some("proc:1".into())
            })),
            now: Some(Arc::new(move || {
                let mut value = clock.lock().unwrap();
                *value += 1.0;
                *value
            })),
        });
        registry.register();
        registry.register();
        assert_eq!(probes.load(std::sync::atomic::Ordering::SeqCst), 1);
        let entry: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("owners/7.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(entry["heartbeatAt"], 2.0);
    }

    #[test]
    fn parse_owner_rejects_malformed_entries() {
        assert_eq!(
            parse_owner(&json!({"pid": 1, "bootId": "b"})),
            Some(owner(1, "b"))
        );
        for invalid in [
            json!(null),
            json!({"bootId": "b"}),
            json!({"pid": "1", "bootId": "b"}),
            json!({"pid": 1, "bootId": ""}),
            json!({"pid": 1}),
            json!({"pid": -1, "bootId": "b"}),
        ] {
            assert_eq!(parse_owner(&invalid), None, "{invalid}");
        }
    }

    #[test]
    fn boot_id_is_stable_and_uuid_shaped() {
        let first = current_process_owner();
        let second = current_process_owner();
        assert_eq!(first.pid, std::process::id());
        assert_eq!(first.boot_id, second.boot_id);
        assert_eq!(first.boot_id.len(), 36);
    }
}
