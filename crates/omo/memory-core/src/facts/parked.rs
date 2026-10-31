//! Move parked queue entries without opening their payloads. Caller holds facts-queue.lock.
use super::{
    failures_schema::{FactsFailureRecord, FactsFailureState, parse_failures_file},
    schema::{FactsQueueLayout, queue_entry_suffix},
};
use std::{
    collections::HashSet,
    fs, io,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};
static MIGRATED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
fn migrated() -> std::sync::MutexGuard<'static, HashSet<PathBuf>> {
    MIGRATED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
pub fn move_parked_entries(
    layout: &FactsQueueLayout,
    records: &[FactsFailureRecord],
    restore: bool,
) -> io::Result<usize> {
    let suffixes: HashSet<_> = records
        .iter()
        .filter(|record| record.state == FactsFailureState::Parked)
        .map(|record| queue_entry_suffix(&record.conversation_id, &record.end_message_id))
        .collect();
    if suffixes.is_empty() {
        return Ok(0);
    }
    // A non-restore move that had been marked migrated re-marks it only once every rename has
    // succeeded, so a failed rename mid-move is retried rather than skipped.
    let was_migrated = !restore && migrated().remove(&layout.queue_dir);
    let (source, target) = if restore {
        (&layout.parked_dir, &layout.queue_dir)
    } else {
        (&layout.queue_dir, &layout.parked_dir)
    };
    let names = match fs::read_dir(source) {
        Ok(names) => names,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if was_migrated {
                migrated().insert(layout.queue_dir.clone());
            }
            return Ok(0);
        }
        Err(error) => return Err(error),
    };
    let pattern = regex::Regex::new(r"^\d{8}T\d{9}Z-[a-f0-9]{12}-[a-f0-9]{8}\.json$")
        .expect("queue filename pattern");
    let mut moved = 0;
    for entry in names {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !pattern.is_match(&name) || !suffixes.contains(&name[19..]) {
            continue;
        }
        if moved == 0 {
            fs::create_dir_all(target)?;
        }
        fs::rename(entry.path(), target.join(name.as_ref()))?;
        moved += 1;
    }
    if was_migrated {
        migrated().insert(layout.queue_dir.clone());
    }
    Ok(moved)
}
pub fn migrate_parked_entries(layout: &FactsQueueLayout) -> io::Result<()> {
    if migrated().contains(&layout.queue_dir) {
        return Ok(());
    }
    match fs::read_to_string(&layout.failures_path) {
        Ok(raw) => {
            let Ok(file) = parse_failures_file(&raw) else {
                return Ok(());
            };
            move_parked_entries(layout, &file.entries, false)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    migrated().insert(layout.queue_dir.clone());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::failures_schema::{FactsFailureReason, render_failures_file};
    fn record(conversation: &str) -> FactsFailureRecord {
        FactsFailureRecord {
            conversation_id: conversation.into(),
            end_message_id: "m1".into(),
            end_snapshot_line: 1,
            state: FactsFailureState::Parked,
            streak: 1,
            first_failure_at: "2026-10-23T00:00:00.000Z".into(),
            last_failure_at: "2026-10-23T00:00:00.000Z".into(),
            last_reason: FactsFailureReason::PayloadEntryOversize,
            last_detail: None,
            last_failure_id: "legacy".into(),
            next_eligible_at: None,
            parked_at: Some("2026-10-23T00:00:00.000Z".into()),
        }
    }
    #[test]
    fn migration_moves_ten_thousand_payloads_without_parsing_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let layout = FactsQueueLayout::new(dir.path().to_owned());
        let records: Vec<_> = (0..10_000)
            .map(|index| record(&format!("parked-{index}")))
            .collect();
        for record in &records {
            fs::write(
                layout.entry_path(&record.conversation_id, "m1", "2026-10-23T00:00:00.000Z"),
                b"unparseable payload retained byte-for-byte",
            )
            .unwrap();
        }
        let live = layout.entry_path("live", "m1", "2026-10-23T00:00:00.000Z");
        fs::write(&live, "live").unwrap();
        fs::write(
            &layout.failures_path,
            render_failures_file(&records, "2026-10-23T00:00:00.000Z"),
        )
        .unwrap();
        migrate_parked_entries(&layout).unwrap();
        migrate_parked_entries(&layout).unwrap();
        assert_eq!(fs::read_dir(&layout.parked_dir).unwrap().count(), 10_000);
        assert_eq!(fs::read_to_string(live).unwrap(), "live");
        for entry in fs::read_dir(&layout.parked_dir).unwrap() {
            assert_eq!(
                fs::read(entry.unwrap().path()).unwrap(),
                b"unparseable payload retained byte-for-byte"
            );
        }
        assert_eq!(
            move_parked_entries(&layout, &records, true).unwrap(),
            10_000
        );
        assert_eq!(fs::read_dir(&layout.parked_dir).unwrap().count(), 0);
    }
    #[test]
    fn corrupt_ledger_does_not_mark_migration_complete() {
        let dir = tempfile::tempdir().unwrap();
        let layout = FactsQueueLayout::new(dir.path().to_owned());
        let record = record("old");
        let path = layout.entry_path("old", "m1", "2026-10-23T00:00:00.000Z");
        fs::write(&path, "opaque").unwrap();
        fs::write(&layout.failures_path, "corrupt").unwrap();
        migrate_parked_entries(&layout).unwrap();
        assert!(path.exists());
        fs::write(
            &layout.failures_path,
            render_failures_file(&[record], "2026-10-23T00:00:00.000Z"),
        )
        .unwrap();
        migrate_parked_entries(&layout).unwrap();
        assert!(!path.exists());
    }

    fn parked_record() -> FactsFailureRecord {
        FactsFailureRecord {
            conversation_id: "conversation".into(),
            end_message_id: "end".into(),
            end_snapshot_line: 1,
            state: FactsFailureState::Parked,
            streak: 1,
            first_failure_at: "2026-10-27T00:00:00.000Z".into(),
            last_failure_at: "2026-10-27T00:00:00.000Z".into(),
            last_reason: FactsFailureReason::ChildExit,
            last_detail: None,
            last_failure_id: "failure".into(),
            next_eligible_at: None,
            parked_at: Some("2026-10-27T00:00:00.000Z".into()),
        }
    }

    #[test]
    fn a_failed_move_leaves_a_migrated_queue_unmarked_so_it_is_retried() {
        let root = tempfile::tempdir().expect("tempdir");
        let layout = FactsQueueLayout::new(root.path().join("queue"));
        fs::create_dir_all(&layout.queue_dir).expect("queue dir");
        let record = parked_record();
        let name = format!(
            "20261027T000000000Z{}",
            queue_entry_suffix(&record.conversation_id, &record.end_message_id)
        );
        fs::write(layout.queue_dir.join(&name), "{}").expect("entry");
        migrated().insert(layout.queue_dir.clone());
        // A file where the parked directory belongs makes the move fail before any rename.
        fs::write(&layout.parked_dir, "blocker").expect("blocker");

        assert!(move_parked_entries(&layout, std::slice::from_ref(&record), false).is_err());
        assert!(!migrated().contains(&layout.queue_dir));

        fs::remove_file(&layout.parked_dir).expect("unblock");
        assert_eq!(
            move_parked_entries(&layout, std::slice::from_ref(&record), false).expect("move"),
            1
        );
        assert!(layout.parked_dir.join(&name).exists());
        migrated().remove(&layout.queue_dir);
    }

    #[test]
    fn a_successful_move_keeps_a_migrated_queue_marked() {
        let root = tempfile::tempdir().expect("tempdir");
        let layout = FactsQueueLayout::new(root.path().join("queue"));
        fs::create_dir_all(&layout.queue_dir).expect("queue dir");
        migrated().insert(layout.queue_dir.clone());
        let record = parked_record();

        assert_eq!(
            move_parked_entries(&layout, std::slice::from_ref(&record), false).expect("move"),
            0
        );
        assert!(migrated().contains(&layout.queue_dir));
        migrated().remove(&layout.queue_dir);
    }
}
