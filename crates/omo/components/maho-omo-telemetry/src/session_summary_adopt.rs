//! Port of `packages/omo-senpi/src/components/telemetry/session-summary-adopt.ts`.
//!
//! Continues the window another process wrote for this session once that process is gone, claiming the file
//! first so two resuming processes never both continue it. A live owner, or a claim another process won,
//! leaves this process its own copy beside the shared file.

use crate::omo_native_session_summary::LiveWindow;
use crate::session_summary_files::remove_file;
use crate::session_summary_store::{
    ConsentScope, ReadResult, RecoveryStorage, WindowRecord, is_record_sendable,
};
use crate::session_summary_window::new_summary_epoch;

/// The arguments a caller's `fresh` constructor may override; `None` keeps the caller's defaults.
#[derive(Debug, Clone, Default)]
pub struct FreshArgs {
    pub key: Option<String>,
    pub epoch: Option<String>,
    pub seq: Option<u64>,
}

pub struct AdoptForeignWindowInput<'a> {
    pub storage: &'a RecoveryStorage,
    pub key: &'a str,
    pub own_copy: &'a str,
    pub stored: &'a WindowRecord,
    pub is_scope_consented: &'a (dyn Fn(&ConsentScope) -> bool + Send + Sync),
    pub fresh: &'a dyn Fn(FreshArgs) -> LiveWindow,
    pub persist: &'a dyn Fn(&LiveWindow),
}

pub fn adopt_foreign_window(input: AdoptForeignWindowInput<'_>) -> LiveWindow {
    let storage = input.storage;
    let alive = storage.is_owner_alive();
    let fresh = input.fresh;
    if alive(&input.stored.owner) {
        return fresh(FreshArgs {
            key: Some(input.own_copy.to_string()),
            ..Default::default()
        });
    }
    let Some(claimed) = storage.claim_window(input.key) else {
        return fresh(FreshArgs {
            key: Some(input.own_copy.to_string()),
            ..Default::default()
        });
    };
    let record = match claimed.record {
        ReadResult::Value(record)
            if is_record_sendable(record.consent, record.scope.as_ref(), input.is_scope_consented) =>
        {
            record
        }
        _ => {
            remove_file(&claimed.claim_path);
            return fresh(FreshArgs::default());
        }
    };
    // A new epoch on every hand-over: if the owner was misjudged dead and still publishes, no (epoch, seq)
    // repeats.
    let epoch = new_summary_epoch();
    let published = record
        .window_id
        .as_deref()
        .is_some_and(|window_id| storage.has_pending(window_id));
    let entry = if published || record.window.is_none() {
        fresh(FreshArgs {
            key: Some(input.key.to_string()),
            epoch: Some(epoch.clone()),
            seq: Some(if published { record.seq + 1 } else { record.seq }),
        })
    } else {
        LiveWindow {
            key: input.key.to_string(),
            epoch,
            seq: record.seq,
            sample_rate: record.sample_rate,
            window_id: record.window_id.clone(),
            window: record.window.clone(),
        }
    };
    (input.persist)(&entry);
    remove_file(&claimed.claim_path);
    entry
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_summary_store::{
        Probes, RecordConsent, create_summary_storage, with_recovery_ops,
    };
    use crate::session_summary_window::open_window;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn record(pid: u32, boot_id: &str, with_window: bool) -> WindowRecord {
        WindowRecord {
            consent: RecordConsent::Enabled,
            scope: Some(ConsentScope {
                cwd: Some("/repo".into()),
                config: None,
            }),
            session_hash: "a".repeat(64),
            session_role: crate::session_role::TelemetrySessionRole::Root,
            owner: crate::session_summary_owner::ProcessOwner {
                pid,
                boot_id: boot_id.into(),
            },
            heartbeat_at: 0.0,
            epoch: Some("0123456789abcdef".into()),
            seq: 4,
            sample_rate: 0.05,
            window_id: with_window.then(|| "01234567-89ab-cdef-0123-456789abcdef".to_string()),
            window: with_window.then(|| open_window(1.0, 1.0)),
        }
    }

    struct Fixture {
        dir: tempfile::TempDir,
        storage: RecoveryStorage,
        alive: Arc<AtomicBool>,
        persisted: Arc<std::sync::Mutex<Vec<LiveWindow>>>,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let alive = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&alive);
        let storage = create_summary_storage(
            dir.path(),
            crate::session_summary_owner::ProcessOwner {
                pid: 1,
                boot_id: "boot-self".into(),
            },
            Probes {
                is_pid_alive: Some(Arc::new(move |_| observed.load(Ordering::SeqCst))),
                process_start_time: Some(Arc::new(|_| None)),
            },
            Some(Arc::new(|| 1000.0)),
        );
        Fixture {
            dir,
            storage: with_recovery_ops(storage),
            alive,
            persisted: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn write_peer_owner(dir: &std::path::Path, pid: u32, boot_id: &str) {
        let path = dir.join("session-summary/owners").join(format!("{pid}.json"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            serde_json::json!({"pid": pid, "bootId": boot_id, "startTime": null, "heartbeatAt": 0.0})
                .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn a_live_owner_leaves_this_process_its_own_copy() {
        let fixture = fixture();
        fixture.alive.store(true, Ordering::SeqCst);
        write_peer_owner(fixture.dir.path(), 2, "boot-peer");
        let fresh = |args: FreshArgs| LiveWindow {
            key: args.key.unwrap_or_else(|| "shared".to_string()),
            epoch: "fresh-epoch-0000".into(),
            seq: 0,
            sample_rate: 0.05,
            window_id: None,
            window: None,
        };
        let persist = |_: &LiveWindow| {};
        let entry = adopt_foreign_window(AdoptForeignWindowInput {
            storage: &fixture.storage,
            key: "shared",
            own_copy: "shared@boot-self",
            stored: &record(2, "boot-peer", true),
            is_scope_consented: &|_| true,
            fresh: &fresh,
            persist: &persist,
        });
        assert_eq!(entry.key, "shared@boot-self");
        assert_eq!(entry.seq, 0);
        assert!(fixture.persisted.lock().unwrap().is_empty());
    }

    #[test]
    fn a_dead_owner_window_is_claimed_continued_in_a_new_epoch() {
        let fixture = fixture();
        let storage = fixture.storage.storage.clone();
        storage.write_window("shared", &record(2, "boot-peer", true)).unwrap();
        let fresh = |args: FreshArgs| LiveWindow {
            key: args.key.unwrap_or_else(|| "shared".to_string()),
            epoch: args.epoch.unwrap_or_else(|| "default-epoch-00".to_string()),
            seq: args.seq.unwrap_or(0),
            sample_rate: 0.05,
            window_id: None,
            window: None,
        };
        let entry = adopt_foreign_window(AdoptForeignWindowInput {
            storage: &fixture.storage,
            key: "shared",
            own_copy: "shared@boot-self",
            stored: &record(2, "boot-peer", true),
            is_scope_consented: &|_| true,
            fresh: &fresh,
            persist: &|entry: &LiveWindow| {
                fixture.persisted.lock().unwrap().push(entry.clone());
            },
        });
        assert_eq!(entry.key, "shared");
        assert_eq!(entry.seq, 4);
        assert_eq!(entry.window_id.as_deref(), Some("01234567-89ab-cdef-0123-456789abcdef"));
        assert_ne!(entry.epoch, "0123456789abcdef");
        assert_eq!(fixture.persisted.lock().unwrap().len(), 1);
        assert!(!fixture.dir.path().join("session-summary/shared.json").exists());
    }

    #[test]
    fn a_published_window_continues_at_the_next_sequence() {
        let fixture = fixture();
        let storage = fixture.storage.storage.clone();
        storage.write_window("shared", &record(2, "boot-peer", true)).unwrap();
        storage
            .write_pending(&crate::session_summary_store::PendingRecord {
                consent: RecordConsent::Enabled,
                scope: None,
                uuid: "01234567-89ab-cdef-0123-456789abcdef".into(),
                session_hash: "a".repeat(64),
                owner: storage.owner.clone(),
                created_at: 0.0,
                properties: serde_json::Map::new(),
            })
            .unwrap();
        let fresh = |args: FreshArgs| LiveWindow {
            key: args.key.unwrap_or_else(|| "shared".to_string()),
            epoch: args.epoch.unwrap_or_else(|| "default-epoch-00".to_string()),
            seq: args.seq.unwrap_or(0),
            sample_rate: 0.05,
            window_id: None,
            window: None,
        };
        let entry = adopt_foreign_window(AdoptForeignWindowInput {
            storage: &fixture.storage,
            key: "shared",
            own_copy: "shared@boot-self",
            stored: &record(2, "boot-peer", true),
            is_scope_consented: &|_| true,
            fresh: &fresh,
            persist: &|_: &LiveWindow| {},
        });
        assert_eq!(entry.seq, 5);
        assert!(entry.window.is_none());
    }

    #[test]
    fn an_opted_out_or_corrupt_window_starts_fresh_and_the_claim_is_removed() {
        let fixture = fixture();
        let storage = fixture.storage.storage.clone();
        storage.write_window("shared", &record(2, "boot-peer", true)).unwrap();
        let fresh = |args: FreshArgs| LiveWindow {
            key: args.key.unwrap_or_else(|| "default-key".to_string()),
            epoch: args.epoch.unwrap_or_else(|| "default-epoch-00".to_string()),
            seq: args.seq.unwrap_or(0),
            sample_rate: 0.05,
            window_id: None,
            window: None,
        };
        let entry = adopt_foreign_window(AdoptForeignWindowInput {
            storage: &fixture.storage,
            key: "shared",
            own_copy: "shared@boot-self",
            stored: &record(2, "boot-peer", true),
            is_scope_consented: &|_| false,
            fresh: &fresh,
            persist: &|_: &LiveWindow| {},
        });
        assert_eq!(entry.key, "default-key");
        assert_eq!(entry.seq, 0);
        assert!(!fixture.dir.path().join("session-summary/shared.json").exists());
        assert!(
            fixture
                .storage
                .leftovers()
                .iter()
                .all(|leftover| !leftover.path.to_string_lossy().contains("shared"))
        );
    }
}
