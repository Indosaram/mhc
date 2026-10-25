//! Port of `packages/omo-senpi/src/components/telemetry/session-summary-optout-purge.ts`.
//!
//! Run by an opted-out process at startup and shutdown. Every record a DEAD owner left that may not be sent
//! now (no `enabled` mark, or its scope is opted out) is deleted: windows, pending events, claims and temps,
//! and dead owners' registry entries. Files of live owners are theirs: each re-checks its own consent.
//! Nothing here registers this process as an owner or writes a file.

use std::path::Path;

use crate::session_summary_files::{JsonRead, link_if_absent, read_json_or_corrupt, remove_file};
use crate::session_summary_owner::parse_owner;
use crate::session_summary_store::{
    ConsentScope, ReadResult, RecoveryStorage, is_record_sendable, parse_pending_record,
    parse_window_record,
};

pub fn purge_opted_out_queue(
    storage: &RecoveryStorage,
    is_scope_consented: &(dyn Fn(&ConsentScope) -> bool + Send + Sync),
) {
    let alive = storage.is_owner_alive();
    let sendable_window = |record: &crate::session_summary_store::WindowRecord| {
        is_record_sendable(record.consent, record.scope.as_ref(), is_scope_consented)
    };
    let sendable_pending = |record: &crate::session_summary_store::PendingRecord| {
        is_record_sendable(record.consent, record.scope.as_ref(), is_scope_consented)
    };

    for leftover in storage.leftovers() {
        if alive(&leftover.owner) {
            continue;
        }
        let record_sendable = match read_json_or_corrupt(&leftover.path) {
            JsonRead::Value(value) => match leftover.kind {
                crate::session_summary_store::LeftoverKind::Temp => None,
                crate::session_summary_store::LeftoverKind::PendingClaim => {
                    parse_pending_record(&value).map(|record| sendable_pending(&record))
                }
                crate::session_summary_store::LeftoverKind::WindowClaim => {
                    parse_window_record(&value).map(|record| sendable_window(&record))
                }
            },
            JsonRead::Missing | JsonRead::Corrupt => None,
        };
        if record_sendable == Some(true) {
            continue;
        }
        remove_file(&leftover.path);
    }

    // A record a live owner holds, or one that may still be sent, is kept. The decision is re-made on the
    // copy the claim (an atomic rename) actually took, so a live writer's rewrite between the read and the
    // delete survives.
    for key in storage.storage.window_keys() {
        let keep = match storage.storage.read_window(&key) {
            ReadResult::Value(record) => alive(&record.owner) || sendable_window(&record),
            ReadResult::Missing | ReadResult::Corrupt => continue,
        };
        if keep {
            continue;
        }
        if let Some(claimed) = storage.claim_window(&key) {
            let keep = match &claimed.record {
                ReadResult::Value(record) => alive(&record.owner) || sendable_window(record),
                ReadResult::Missing | ReadResult::Corrupt => false,
            };
            settle_claim(&claimed.claim_path, keep);
        }
    }

    for uuid in storage.storage.pending_uuids() {
        let keep = match storage.read_pending(&uuid) {
            ReadResult::Value(record) => alive(&record.owner) || sendable_pending(&record),
            ReadResult::Missing | ReadResult::Corrupt => continue,
        };
        if keep {
            continue;
        }
        if let Some(claimed) = storage.claim_pending(&uuid) {
            let keep = match &claimed.record {
                ReadResult::Value(record) => alive(&record.owner) || sendable_pending(record),
                ReadResult::Missing | ReadResult::Corrupt => false,
            };
            settle_claim(&claimed.claim_path, keep);
        }
    }

    // Asking about a dead owner removes its registry entry.
    for name in crate::session_summary_files::list_names(&storage.storage.dir.join("owners")) {
        let entry = read_json_or_corrupt(&storage.storage.dir.join("owners").join(&name));
        if let Some(owner) = entry.value().as_ref().and_then(parse_owner)
            && owner.pid != storage.storage.owner.pid
        {
            let _dead = alive(&owner);
        }
    }
}

/// Hands a kept record back without ever replacing a newer file at its name (exclusive link), else deletes it.
fn settle_claim(claim_path: &Path, keep: bool) {
    if keep {
        let name = claim_path.to_string_lossy();
        if let Some((base, _)) = name.rsplit_once(".claimed-") {
            let _restored = link_if_absent(claim_path, Path::new(base));
        }
    }
    remove_file(claim_path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_summary_store::{
        PendingRecord, Probes, RecordConsent, WindowRecord, create_summary_storage,
        with_recovery_ops,
    };
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn owner(pid: u32, boot_id: &str) -> crate::session_summary_owner::ProcessOwner {
        crate::session_summary_owner::ProcessOwner {
            pid,
            boot_id: boot_id.into(),
        }
    }

    struct Fixture {
        dir: tempfile::TempDir,
        storage: RecoveryStorage,
        alive: Arc<AtomicBool>,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let alive = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&alive);
        let storage = create_summary_storage(
            dir.path(),
            owner(1, "boot-self"),
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
        }
    }

    fn window(consent: RecordConsent, pid: u32, boot_id: &str) -> WindowRecord {
        WindowRecord {
            consent,
            scope: Some(ConsentScope {
                cwd: Some("/repo".into()),
                config: None,
            }),
            session_hash: "a".repeat(64),
            session_role: crate::session_role::TelemetrySessionRole::Root,
            owner: owner(pid, boot_id),
            heartbeat_at: 0.0,
            epoch: None,
            seq: 0,
            sample_rate: 0.05,
            window_id: None,
            window: None,
        }
    }

    #[test]
    fn dead_owner_windows_and_pending_events_go_live_ones_stay() {
        let fixture = fixture();
        fixture
            .storage
            .storage
            .write_window("dead", &window(RecordConsent::Enabled, 2, "boot-dead"))
            .unwrap();
        fixture
            .storage
            .storage
            .write_window("live", &window(RecordConsent::Enabled, 3, "boot-live"))
            .unwrap();
        fixture
            .storage
            .storage
            .write_pending(&PendingRecord {
                consent: RecordConsent::Enabled,
                scope: None,
                uuid: "01234567-89ab-cdef-0123-456789abcdef".into(),
                session_hash: "h".into(),
                owner: owner(2, "boot-dead"),
                created_at: 0.0,
                properties: serde_json::Map::new(),
            })
            .unwrap();
        // The live owner has its own registry entry, so liveness keeps it.
        let owners = fixture.dir.path().join("session-summary/owners");
        std::fs::create_dir_all(&owners).unwrap();
        std::fs::write(
            owners.join("3.json"),
            json!({"pid": 3, "bootId": "boot-live", "startTime": null, "heartbeatAt": 0.0})
                .to_string(),
        )
        .unwrap();
        fixture.alive.store(true, Ordering::SeqCst);
        purge_opted_out_queue(&fixture.storage, &|_| false);
        assert_eq!(
            fixture.storage.storage.window_keys(),
            vec!["live".to_string()]
        );
        assert!(fixture.storage.storage.pending_uuids().is_empty());
    }

    #[test]
    fn a_sendable_dead_owner_claim_is_kept_and_an_unconsented_one_dropped() {
        let fixture = fixture();
        let dir = fixture.dir.path().join("session-summary");
        std::fs::create_dir_all(&dir).unwrap();
        let keep = dir.join("keep.json.claimed-2-boot-dead");
        std::fs::write(
            &keep,
            crate::session_summary_store::window_record_to_json(&window(
                RecordConsent::Enabled,
                2,
                "boot-dead",
            ))
            .to_string(),
        )
        .unwrap();
        let drop = dir.join("drop.json.claimed-2-boot-dead");
        std::fs::write(
            &drop,
            crate::session_summary_store::window_record_to_json(&window(
                RecordConsent::Unconsented,
                2,
                "boot-dead",
            ))
            .to_string(),
        )
        .unwrap();
        // A live owner's claim survives untouched.
        fixture.alive.store(false, Ordering::SeqCst);
        purge_opted_out_queue(&fixture.storage, &|scope: &ConsentScope| {
            scope.cwd.as_deref() == Some("/repo")
        });
        assert!(keep.exists());
        assert!(!drop.exists());
        assert!(!dir.join("drop.json").exists());
    }

    #[test]
    fn live_owner_claims_are_never_touched() {
        let fixture = fixture();
        let dir = fixture.dir.path().join("session-summary");
        std::fs::create_dir_all(&dir).unwrap();
        let claim = dir.join("live.json.claimed-2-boot-live");
        std::fs::write(
            &claim,
            crate::session_summary_store::window_record_to_json(&window(
                RecordConsent::Unconsented,
                2,
                "boot-live",
            ))
            .to_string(),
        )
        .unwrap();
        let owners = dir.join("owners");
        std::fs::create_dir_all(&owners).unwrap();
        std::fs::write(
            owners.join("2.json"),
            json!({"pid": 2, "bootId": "boot-live", "startTime": null, "heartbeatAt": 0.0}).to_string(),
        )
        .unwrap();
        fixture.alive.store(true, Ordering::SeqCst);
        purge_opted_out_queue(&fixture.storage, &|_| false);
        assert!(claim.exists());
    }
}
