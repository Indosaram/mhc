//! Port of senpi `terminal/file-monitor-record.ts`: a live file watch's record - its options, its
//! proven identity and its check-loop state. Native shape: the registry keeps the snapshot half
//! (stable id, start, deadline, fire count) in `MonitorSnapshotEntry` and drives the poll loop, so
//! the record holds what a check pass needs.
use crate::terminal_manifest_model::{FileEvent, TerminalManifestCheckpoint};
use std::path::PathBuf;

pub struct FileMonitorRecord {
    pub id: String,
    pub description: String,
    pub(crate) path: PathBuf,
    /// Upstream `canonicalParent`: the proven parent every check re-proves.
    pub(crate) parent: PathBuf,
    pub(crate) event: FileEvent,
    /// Upstream `device`/`inode`: the identity a present target must keep.
    pub(crate) identity: (f64, f64),
    pub checkpoint: TerminalManifestCheckpoint,
    pub paused: bool,
    pub settled: bool,
    pub(crate) reservation: Option<crate::manager::CapacityReservation>,
}

/// What `newFileRecord` needs from a proven registration.
pub struct NewFileRecordInput {
    pub id: String,
    pub description: String,
    pub path: PathBuf,
    pub parent: PathBuf,
    pub event: FileEvent,
    /// The initial digest and stat; an absent target is `present: false`.
    pub initial: TerminalManifestCheckpoint,
}

/// `newFileRecord`: a fresh record, unpaused and unsettled, whose identity is the initial target's.
pub fn new_file_record(input: NewFileRecordInput) -> FileMonitorRecord {
    let NewFileRecordInput {
        id,
        description,
        path,
        parent,
        event,
        initial,
    } = input;
    FileMonitorRecord {
        id,
        description,
        path,
        parent,
        event,
        identity: (initial.dev, initial.ino),
        checkpoint: initial,
        paused: false,
        settled: false,
        reservation: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_record_takes_the_initial_identity_and_is_live() {
        let initial = TerminalManifestCheckpoint {
            dev: 7.0,
            ino: 42.0,
            size: 3.0,
            mtime_ms: 1.0,
            digest: "d".into(),
            present: true,
        };
        let record = new_file_record(NewFileRecordInput {
            id: "watch_1".into(),
            description: "watch".into(),
            path: PathBuf::from("/tmp/x/file"),
            parent: PathBuf::from("/tmp/x"),
            event: FileEvent::Modify,
            initial: initial.clone(),
        });
        assert_eq!(record.identity, (7.0, 42.0));
        assert_eq!(record.checkpoint, initial);
        assert!(!record.paused && !record.settled);
        assert!(record.reservation.is_none());
    }
}
