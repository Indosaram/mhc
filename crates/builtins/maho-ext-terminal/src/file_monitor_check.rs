//! Port of senpi `terminal/file-monitor-check.ts`: one check pass of a file watch - re-prove the
//! target's identity (the parent unchanged, the same inode) without open(2) on a path component,
//! digest it, and decide whether the watched event happened. The watch settles on a failed proof
//! and fires (then completes) on its event.
use crate::file_monitor::realpath_without_open;
use crate::file_monitor_record::FileMonitorRecord;
use crate::terminal_manifest_model::FileEvent;

/// Upstream `FileCheckActions`: what a check pass decided.
#[derive(Debug, PartialEq, Eq)]
pub enum FileCheckOutcome {
    /// Nothing happened (or the watch is paused or settled).
    Quiet,
    /// The watched event happened: emit this line, then complete the watch.
    Fire(String),
}

/// `checkFileOnce`: an `Err` is a failed identity proof; the caller settles the watch with
/// `watcher error: <error>`.
pub fn check_file_once(record: &mut FileMonitorRecord) -> std::io::Result<FileCheckOutcome> {
    if record.paused || record.settled {
        return Ok(FileCheckOutcome::Quiet);
    }
    let raw_parent = record.path.parent().expect("registered file parent");
    if realpath_without_open(raw_parent) != record.parent {
        return Err(std::io::Error::other(format!(
            "monitored parent changed: {}",
            raw_parent.display()
        )));
    }
    let current =
        crate::durable_file::file_checkpoint_with_identity(&record.path, Some(record.identity))?;
    let changed = match record.event {
        FileEvent::Create => !record.checkpoint.present && current.present,
        FileEvent::Modify => {
            record.checkpoint.present
                && current.present
                && (record.checkpoint.mtime_ms != current.mtime_ms
                    || record.checkpoint.size != current.size
                    || record.checkpoint.digest != current.digest)
        }
    };
    if !record.checkpoint.present && current.present {
        record.identity = (current.dev, current.ino);
    }
    record.checkpoint = current;
    if !changed {
        return Ok(FileCheckOutcome::Quiet);
    }
    let event = match record.event {
        FileEvent::Create => "create",
        FileEvent::Modify => "modify",
    };
    Ok(FileCheckOutcome::Fire(format!(
        "{event} {}",
        record.path.display()
    )))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::file_monitor::FileMonitor;

    #[test]
    fn a_modify_pass_fires_once_on_a_content_change_and_is_quiet_otherwise() -> std::io::Result<()>
    {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("watched");
        std::fs::write(&path, b"before")?;
        let mut record = FileMonitor::register(
            "watch_1".into(),
            "modified".into(),
            &path,
            FileEvent::Modify,
            None,
        )?;
        assert_eq!(check_file_once(&mut record)?, FileCheckOutcome::Quiet);
        std::fs::write(&path, b"after!")?;
        assert_eq!(
            check_file_once(&mut record)?,
            FileCheckOutcome::Fire(format!("modify {}", path.display()))
        );
        record.settled = true;
        std::fs::write(&path, b"again!!")?;
        assert_eq!(check_file_once(&mut record)?, FileCheckOutcome::Quiet);
        Ok(())
    }
}
