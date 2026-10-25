//! Live completion deadline of a supervised run (`worker/ledger-deadline-reader.ts`).
//!
//! The parent reads the deadline from the run ledger on every state change instead of arming one
//! fixed timeout at launch: a queued supervisor has no deadline at all, and an admitted one carries
//! the deadline the supervisor rebased by the queue duration. The read runs inside timer and watcher
//! callbacks, where a failure would be a panic in the user's session, so a failed read (a torn or
//! missing file) keeps the last deadline it read and the next read retries.

use std::path::{Path, PathBuf};

use super::run_artifacts::read_run_json;

/// Reads `runDir/ledger.json` and returns `deadlineAt + marginMs`, or `None` while the run is queued.
pub fn create_ledger_deadline_reader(
    run_dir: &Path,
    margin_ms: f64,
) -> impl FnMut() -> Option<f64> {
    let ledger_path: PathBuf = run_dir.join("ledger.json");
    let mut last_read: Option<f64> = None;
    move || {
        if let Ok(current) = read_run_json::<serde_json::Value>(&ledger_path) {
            if current["queued"] == true {
                last_read = None;
            } else if let Some(deadline) = current["deadlineAt"]
                .as_f64()
                .filter(|deadline| deadline.is_finite())
            {
                last_read = Some(deadline + margin_ms);
            }
        }
        last_read
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn write(run_dir: &Path, value: serde_json::Value) {
        super::super::run_artifacts::write_run_json_atomic(
            &run_dir.join("ledger.json"),
            &value,
            0o600,
        )
        .unwrap();
    }

    #[test]
    fn a_queued_run_has_no_deadline_and_an_admitted_one_adds_the_margin() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            serde_json::json!({"version":1,"queued":true,"deadlineAt":100.0}),
        );
        let mut read = create_ledger_deadline_reader(root.path(), 5.0);
        assert_eq!(read(), None);
        write(
            root.path(),
            serde_json::json!({"version":1,"queued":false,"deadlineAt":100.0}),
        );
        assert_eq!(read(), Some(105.0));
    }

    #[test]
    fn a_successful_but_invalid_ledger_read_preserves_the_last_deadline() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), serde_json::json!({"deadlineAt":100.0}));
        let mut read = create_ledger_deadline_reader(root.path(), 5.0);
        assert_eq!(read(), Some(105.0));
        write(root.path(), serde_json::json!({"deadlineAt":"invalid"}));
        assert_eq!(read(), Some(105.0));
    }

    #[test]
    fn a_failed_read_keeps_the_last_good_deadline() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            serde_json::json!({"version":1,"deadlineAt":100.0}),
        );
        let mut read = create_ledger_deadline_reader(root.path(), 5.0);
        assert_eq!(read(), Some(105.0));
        std::fs::write(root.path().join("ledger.json"), "{torn").unwrap();
        assert_eq!(read(), Some(105.0));
        std::fs::remove_file(root.path().join("ledger.json")).unwrap();
        assert_eq!(read(), Some(105.0));
    }

    #[test]
    fn the_reader_is_usable_from_a_shared_callback() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            serde_json::json!({"version":1,"deadlineAt":7.0}),
        );
        let read = RefCell::new(create_ledger_deadline_reader(root.path(), 1.0));
        assert_eq!((read.borrow_mut())(), Some(8.0));
    }
}
