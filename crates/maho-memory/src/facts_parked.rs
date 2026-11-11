//! Parked batch files live beside the queue so a permanently-failing range stops being retried without
//! being destroyed.
//!
//! Spec: `docs/cleanroom/specs/memory-facts-parked.md`.
//!
//! Moves are matched by **file name suffix**, never by reading the payload: a parked batch is exactly the
//! batch whose payload could not be parsed, so parsing it to decide where it goes would fail the same way.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;

use crate::facts_queue::{FailureState, Failures};
use crate::facts_schema::{FactsQueueLayout, queue_entry_suffix};

const TIMESTAMP_LEN: usize = 19;

/// `<ts>-<convHash12>-<endHash8>.json`; `None` for anything else in the queue directory.
fn suffix_of(name: &str) -> Option<&str> {
    let rest = name.strip_suffix(".json")?;
    let bytes = rest.as_bytes();
    if bytes.len() != TIMESTAMP_LEN + 1 + 12 + 1 + 8 {
        return None;
    }
    let digits = |slice: &[u8]| slice.iter().all(u8::is_ascii_digit);
    let hex = |slice: &[u8]| slice.iter().all(u8::is_ascii_hexdigit);
    let well_formed = digits(&bytes[0..8])
        && bytes[8] == b'T'
        && digits(&bytes[9..18])
        && bytes[18] == b'Z'
        && bytes[19] == b'-'
        && hex(&bytes[20..32])
        && bytes[32] == b'-'
        && hex(&bytes[33..41]);
    well_formed.then(|| &name[TIMESTAMP_LEN..])
}

/// `restore` reverses only the direction, so one matcher serves both moves: a batch is parked, later
/// restored for an explicit retry, and both must find the same set of files.
///
/// A missing source directory is `0`, not an error: nothing is parked yet on a fresh store, and the sweep
/// that calls this runs on every extraction cycle.
///
/// **Only the first moved file creates the target directory.** Doing it per file costs one syscall per
/// parked batch; skipping it entirely fails the first rename on a store that never parked anything.
pub fn move_parked_entries(
    layout: &FactsQueueLayout,
    entries: &[crate::facts_queue::FailureEntry],
    restore: bool,
) -> io::Result<usize> {
    let suffixes: HashSet<String> = entries
        .iter()
        .filter(|entry| entry.state == FailureState::Parked)
        .map(|entry| queue_entry_suffix(&entry.conversation_id, &entry.end_message_id))
        .collect();
    if suffixes.is_empty() {
        return Ok(0);
    }
    let (source, target) = if restore {
        (&layout.parked_dir, &layout.queue_dir)
    } else {
        (&layout.queue_dir, &layout.parked_dir)
    };
    let names = match fs::read_dir(source) {
        Ok(names) => names,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut moved = 0;
    for entry in names {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(suffix) = suffix_of(&name) else {
            continue;
        };
        if !suffixes.contains(suffix) {
            continue;
        }
        if moved == 0 {
            fs::create_dir_all(target)?;
        }
        fs::rename(entry.path(), target.join(name.as_ref()))?;
        moved += 1;
    }
    Ok(moved)
}

/// Whether a path is a queue file name. Used to repair a store written before parking existed.
pub fn is_parked_file(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| suffix_of(&name.to_string_lossy()).is_some())
}

/// The flag matters because the ledger write and the file move are not one transaction: a crash between
/// them leaves a parked record whose file is still in the queue. Retrying is the repair, so the mark is
/// only set **after** the move succeeds — a failed move must be attempted again rather than skipped for
/// the life of the process.
#[derive(Debug, Default)]
pub struct ParkedMigration {
    migrated: HashSet<std::path::PathBuf>,
}

impl ParkedMigration {
    pub fn run(&mut self, layout: &FactsQueueLayout) -> io::Result<bool> {
        if self.migrated.contains(&layout.queue_dir) {
            return Ok(false);
        }
        match fs::read_to_string(&layout.failures_path) {
            Ok(raw) => {
                if let Ok(failures) = Failures::parse(&raw) {
                    move_parked_entries(layout, &failures.entries, false)?;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.migrated.insert(layout.queue_dir.clone());
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts_queue::FailureEntry;
    use tempfile::TempDir;

    fn layout(dir: &TempDir) -> FactsQueueLayout {
        let layout = FactsQueueLayout::new(dir.path().join("queue"));
        fs::create_dir_all(&layout.queue_dir).expect("queue dir");
        layout
    }

    fn parked(conversation: &str, end: &str) -> FailureEntry {
        FailureEntry {
            conversation_id: conversation.to_owned(),
            end_message_id: end.to_owned(),
            end_snapshot_line: 1,
            state: FailureState::Parked,
            streak: 5,
            first_failure_at: "2026-10-23T00:00:00.000Z".to_owned(),
            last_failure_at: "2026-10-23T00:00:00.000Z".to_owned(),
            last_reason: "payload_entry_oversize".to_owned(),
            last_failure_id: "f1".to_owned(),
            next_eligible_at: None,
        }
    }

    fn file_name(conversation: &str, end: &str, at: &str) -> String {
        format!(
            "{}-{}-{}.json",
            crate::facts_schema::queue_timestamp(at),
            crate::facts_schema::queue_conversation_hash(conversation),
            crate::facts_schema::queue_entry_suffix(conversation, end)
                .trim_start_matches('-')
                .split_once('-')
                .expect("suffix")
                .1
                .trim_end_matches(".json")
        )
    }

    fn write(dir: &Path, name: &str) {
        fs::write(dir.join(name), b"{\"payload\":\"not parsed\"}").expect("write");
    }

    #[test]
    fn only_recognised_queue_names_parse() {
        assert_eq!(
            suffix_of("20261023T000000000Z-abcdef012345-01234567.json"),
            Some("-abcdef012345-01234567.json")
        );
        for name in [
            "20261023T000000000Z-abcdef012345-01234567.json.tmp",
            "claims.json",
            "20261023T000000000X-abcdef012345-01234567.json",
            "20261023T000000000Z-abcdef01234z-01234567.json",
            "20261023T000000000Z-abcdef012345-0123456.json",
        ] {
            assert_eq!(suffix_of(name), None, "name: {name}");
        }
    }

    #[test]
    fn parking_moves_a_matching_batch_without_reading_it() {
        let dir = TempDir::new().expect("tmp");
        let layout = layout(&dir);
        let name = file_name("c1", "m1", "2026-10-23T00:00:00.000Z");
        write(&layout.queue_dir, &name);
        assert_eq!(
            move_parked_entries(&layout, &[parked("c1", "m1")], false).expect("parks"),
            1
        );
        assert!(layout.parked_dir.join(&name).exists());
        assert!(!layout.queue_dir.join(&name).exists());
    }

    #[test]
    fn another_conversations_batch_stays_in_the_queue() {
        let dir = TempDir::new().expect("tmp");
        let layout = layout(&dir);
        let keep = file_name("c2", "m9", "2026-10-23T00:00:00.000Z");
        write(&layout.queue_dir, &keep);
        assert_eq!(
            move_parked_entries(&layout, &[parked("c1", "m1")], false).expect("parks"),
            0
        );
        assert!(layout.queue_dir.join(&keep).exists());
    }

    #[test]
    fn a_retrying_record_is_not_parked() {
        let dir = TempDir::new().expect("tmp");
        let layout = layout(&dir);
        let name = file_name("c1", "m1", "2026-10-23T00:00:00.000Z");
        write(&layout.queue_dir, &name);
        let mut entry = parked("c1", "m1");
        entry.state = FailureState::Retrying;
        assert_eq!(
            move_parked_entries(&layout, &[entry], false).expect("parks"),
            0
        );
        assert!(layout.queue_dir.join(&name).exists());
    }

    #[test]
    fn a_temporary_file_is_never_moved() {
        let dir = TempDir::new().expect("tmp");
        let layout = layout(&dir);
        let name = format!("{}.tmp", file_name("c1", "m1", "2026-10-23T00:00:00.000Z"));
        write(&layout.queue_dir, &name);
        assert_eq!(
            move_parked_entries(&layout, &[parked("c1", "m1")], false).expect("parks"),
            0
        );
    }

    #[test]
    fn restoring_returns_the_batch_to_the_queue() {
        let dir = TempDir::new().expect("tmp");
        let layout = layout(&dir);
        let name = file_name("c1", "m1", "2026-10-23T00:00:00.000Z");
        write(&layout.queue_dir, &name);
        move_parked_entries(&layout, &[parked("c1", "m1")], false).expect("parks");
        assert_eq!(
            move_parked_entries(&layout, &[parked("c1", "m1")], true).expect("restores"),
            1
        );
        assert!(layout.queue_dir.join(&name).exists());
        assert!(!layout.parked_dir.join(&name).exists());
    }

    #[test]
    fn parking_twice_moves_nothing_the_second_time() {
        let dir = TempDir::new().expect("tmp");
        let layout = layout(&dir);
        let name = file_name("c1", "m1", "2026-10-23T00:00:00.000Z");
        write(&layout.queue_dir, &name);
        assert_eq!(
            move_parked_entries(&layout, &[parked("c1", "m1")], false).expect("parks"),
            1
        );
        assert_eq!(
            move_parked_entries(&layout, &[parked("c1", "m1")], false).expect("parks"),
            0
        );
        assert!(layout.parked_dir.join(&name).exists());
    }

    #[test]
    fn a_missing_source_directory_moves_nothing() {
        let dir = TempDir::new().expect("tmp");
        let layout = layout(&dir);
        assert_eq!(
            move_parked_entries(&layout, &[parked("c1", "m1")], true).expect("restores"),
            0
        );
    }

    #[test]
    fn no_parked_records_leaves_every_file_alone() {
        let dir = TempDir::new().expect("tmp");
        let layout = layout(&dir);
        let name = file_name("c1", "m1", "2026-10-23T00:00:00.000Z");
        write(&layout.queue_dir, &name);
        assert_eq!(move_parked_entries(&layout, &[], false).expect("parks"), 0);
        assert!(layout.queue_dir.join(&name).exists());
    }

    #[test]
    fn the_migration_parks_from_the_ledger_once_per_directory() {
        let dir = TempDir::new().expect("tmp");
        let layout = layout(&dir);
        let name = file_name("c1", "m1", "2026-10-23T00:00:00.000Z");
        write(&layout.queue_dir, &name);
        let failures = Failures {
            version: 1,
            updated_at: "2026-10-23T00:00:00.000Z".to_owned(),
            entries: vec![parked("c1", "m1")],
        };
        fs::write(&layout.failures_path, failures.render().expect("renders")).expect("ledger");
        let mut migration = ParkedMigration::default();
        assert!(migration.run(&layout).expect("runs"));
        assert!(layout.parked_dir.join(&name).exists());
        assert!(!migration.run(&layout).expect("runs"));
    }

    #[test]
    fn a_failed_move_is_retried_rather_than_marked_done() {
        let dir = TempDir::new().expect("tmp");
        let layout = layout(&dir);
        let failures = Failures {
            version: 1,
            updated_at: "2026-10-23T00:00:00.000Z".to_owned(),
            entries: vec![parked("c1", "m1")],
        };
        fs::write(&layout.failures_path, failures.render().expect("renders")).expect("ledger");
        write(
            &layout.queue_dir,
            &file_name("c1", "m1", "2026-10-23T00:00:00.000Z"),
        );
        // A live file where the parked directory must go makes create_dir_all fail.
        fs::write(&layout.parked_dir, b"in the way").expect("blocker");
        let mut migration = ParkedMigration::default();
        assert!(migration.run(&layout).is_err());
        assert!(migration.run(&layout).is_err(), "must be attempted again");
    }

    #[test]
    fn a_queue_path_is_recognised_and_a_ledger_is_not() {
        assert!(is_parked_file(Path::new(
            "/q/20261023T000000000Z-abcdef012345-01234567.json"
        )));
        assert!(!is_parked_file(Path::new("/q/claims.json")));
    }
}
