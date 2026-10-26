//! Opt-out only removes verified regular files whose names the spool itself creates.
use crate::otlp_spool_names::{QUARANTINE_DIR, is_spool_file_name};
use crate::types::{
    TelemetryDiagnosticEvent, TelemetryDiagnosticInput, TelemetryDiagnostics,
    TelemetrySpoolEntryKind,
};
use std::{fs, io, path::Path};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurgeOutcome {
    Purged,
    Incomplete,
    Absent,
    Refused,
}
pub struct PurgeReport<'a> {
    pub on_skipped: &'a dyn Fn(TelemetrySpoolEntryKind),
    pub on_failed: &'a dyn Fn(&str),
}
pub type Unlink<'a> = &'a dyn Fn(&Path) -> io::Result<()>;
pub type OwnerOf<'a> = &'a dyn Fn(&Path, u32) -> u32;
#[derive(Default)]
pub struct PurgeSeams<'a> {
    pub unlink: Option<Unlink<'a>>,
    pub owner_of: Option<OwnerOf<'a>>,
}
fn owned(path: &Path, stat: &fs::Metadata, seams: &PurgeSeams<'_>) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let uid = seams.owner_of.map_or(stat.uid(), |f| f(path, stat.uid()));
        #[allow(unsafe_code)]
        // SAFETY: geteuid takes no pointers and has no preconditions.
        let current = unsafe { libc::geteuid() };
        uid == current
    }
    #[cfg(not(unix))]
    {
        let _ = (path, stat, seams);
        true
    }
}
fn real_directory(path: &Path, seams: &PurgeSeams<'_>) -> io::Result<Option<bool>> {
    match fs::symlink_metadata(path) {
        Ok(stat) => Ok(Some(
            stat.is_dir() && !stat.file_type().is_symlink() && owned(path, &stat, seams),
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        b.is_file() && a.dev() == b.dev() && a.ino() == b.ino()
    }
    #[cfg(windows)]
    {
        // Volume serial and file index are unstable in std; creation and last-write
        // FILETIMEs plus size identify a replaced entry on the stable API.
        use std::os::windows::fs::MetadataExt;
        b.is_file()
            && a.file_size() == b.file_size()
            && a.creation_time() == b.creation_time()
            && a.last_write_time() == b.last_write_time()
    }
    #[cfg(not(any(unix, windows)))]
    {
        b.is_file() && a.len() == b.len() && a.modified().ok() == b.modified().ok()
    }
}
fn errno_code(e: &io::Error) -> Option<&'static str> {
    match e.raw_os_error() {
        Some(libc::EPERM) => Some("EPERM"),
        Some(libc::EACCES) => Some("EACCES"),
        Some(libc::EBUSY) => Some("EBUSY"),
        Some(libc::EISDIR) => Some("EISDIR"),
        Some(libc::ENOTDIR) => Some("ENOTDIR"),
        _ if e.kind() == io::ErrorKind::PermissionDenied => Some("EACCES"),
        _ => None,
    }
}
fn unlink_owned(dir: &Path, report: &PurgeReport<'_>, seams: &PurgeSeams<'_>) -> io::Result<usize> {
    let mut failed = 0;
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        if !is_spool_file_name(&entry.file_name().to_string_lossy()) {
            continue;
        }
        let path = entry.path();
        let checked = match fs::symlink_metadata(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        let skipped = if checked.file_type().is_symlink() {
            Some(TelemetrySpoolEntryKind::Symlink)
        } else if checked.is_dir() {
            Some(TelemetrySpoolEntryKind::Directory)
        } else if !checked.is_file() {
            Some(TelemetrySpoolEntryKind::Other)
        } else if !owned(&path, &checked, seams) {
            Some(TelemetrySpoolEntryKind::ForeignOwner)
        } else {
            None
        };
        if let Some(kind) = skipped {
            (report.on_skipped)(kind);
            continue;
        }
        let result = match seams.unlink {
            Some(f) => f(&path),
            None => fs::remove_file(&path),
        };
        if let Err(error) = result {
            if error.kind() == io::ErrorKind::NotFound {
                continue;
            }
            let Some(code) = errno_code(&error) else {
                return Err(error);
            };
            match fs::symlink_metadata(&path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
                Ok(now) if !same_file(&checked, &now) => {
                    (report.on_skipped)(TelemetrySpoolEntryKind::Other)
                }
                Ok(_) => {
                    failed += 1;
                    (report.on_failed)(code);
                }
            }
        }
    }
    Ok(failed)
}
fn remove_if_empty(dir: &Path) -> io::Result<()> {
    match fs::remove_dir(dir) {
        Ok(()) => Ok(()),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::DirectoryNotEmpty
                    | io::ErrorKind::AlreadyExists
            ) =>
        {
            Ok(())
        }
        Err(e) => Err(e),
    }
}
pub fn purge_spool(
    dir: &Path,
    root: &Path,
    report: &PurgeReport<'_>,
    seams: &PurgeSeams<'_>,
) -> io::Result<PurgeOutcome> {
    match real_directory(dir, seams)? {
        None => return Ok(PurgeOutcome::Absent),
        Some(false) => return Ok(PurgeOutcome::Refused),
        Some(true) => {}
    }
    let real_root = match fs::canonicalize(root) {
        Ok(p) => p,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(PurgeOutcome::Refused);
        }
        Err(e) => return Err(e),
    };
    let real_dir = fs::canonicalize(dir)?;
    if real_dir == real_root || !real_dir.starts_with(&real_root) {
        return Ok(PurgeOutcome::Refused);
    }
    let quarantine = dir.join(QUARANTINE_DIR);
    if real_directory(&quarantine, seams)? == Some(false) {
        return Ok(PurgeOutcome::Refused);
    }
    let failed = unlink_owned(dir, report, seams)? + unlink_owned(&quarantine, report, seams)?;
    remove_if_empty(&quarantine)?;
    remove_if_empty(dir)?;
    Ok(if failed > 0 {
        PurgeOutcome::Incomplete
    } else {
        PurgeOutcome::Purged
    })
}
pub fn purge_report(
    diagnostics: Option<&TelemetryDiagnostics>,
    source: &str,
    kind: Option<TelemetrySpoolEntryKind>,
    error_code: Option<&str>,
) {
    if let Some(sink) = diagnostics {
        sink(&TelemetryDiagnosticInput {
            event: if kind.is_some() {
                TelemetryDiagnosticEvent::TelemetrySpoolPurgeSkipped
            } else {
                TelemetryDiagnosticEvent::TelemetrySpoolPurgeFailed
            },
            source: source.into(),
            error: None,
            error_kind: None,
            entry_kind: kind,
            error_code: error_code.map(str::to_owned),
            count: None,
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::otlp_spool_file::Signal;
    use crate::otlp_spool_names::active_name;
    #[test]
    fn purge_is_narrow_and_reports_unlink_failure() -> io::Result<()> {
        let root = tempfile::tempdir()?;
        let dir = root.path().join("spool");
        fs::create_dir(&dir)?;
        let name = active_name(Signal::Logs, 1, &"a".repeat(32));
        fs::write(dir.join(&name), "record")?;
        fs::write(dir.join("notes"), "keep")?;
        let codes = std::cell::RefCell::new(Vec::new());
        let skipped = |_| panic!("unexpected skip");
        let failed = |code: &str| codes.borrow_mut().push(code.to_owned());
        let report = PurgeReport {
            on_skipped: &skipped,
            on_failed: &failed,
        };
        let deny = |_: &Path| Err(io::Error::from_raw_os_error(libc::EACCES));
        assert_eq!(
            purge_spool(
                &dir,
                root.path(),
                &report,
                &PurgeSeams {
                    unlink: Some(&deny),
                    owner_of: None
                }
            )?,
            PurgeOutcome::Incomplete
        );
        assert_eq!(*codes.borrow(), vec!["EACCES"]);
        assert_eq!(
            purge_spool(&dir, root.path(), &report, &PurgeSeams::default())?,
            PurgeOutcome::Purged
        );
        assert!(dir.join("notes").exists());
        assert!(!dir.join(name).exists());
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    fn refuses_symlink_directory_without_deleting() -> io::Result<()> {
        let root = tempfile::tempdir()?;
        let actual = root.path().join("actual");
        fs::create_dir(&actual)?;
        let link = root.path().join("spool");
        std::os::unix::fs::symlink(&actual, &link)?;
        let skip = |_| {};
        let fail = |_: &str| {};
        assert_eq!(
            purge_spool(
                &link,
                root.path(),
                &PurgeReport {
                    on_skipped: &skip,
                    on_failed: &fail
                },
                &PurgeSeams::default()
            )?,
            PurgeOutcome::Refused
        );
        Ok(())
    }
}
