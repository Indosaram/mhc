//! Opt-out only removes verified regular files whose names the spool itself creates.
use crate::otlp_pid_alive::is_pid_alive;
use crate::otlp_spool_names::{
    QUARANTINE_DIR, SpoolEntry, is_spool_file_name, owners_dir, parse_spool_name,
};
use crate::otlp_transport_types::{CreateOtlpTelemetryTransportInput, IsPidAlive, NowMs};
use crate::types::{
    TelemetryDiagnosticErrorKind, TelemetryDiagnosticEvent, TelemetryDiagnosticInput,
    TelemetryDiagnostics, TelemetryError, TelemetrySpoolEntryKind,
};
use std::{
    fs, io,
    path::{Path, PathBuf},
};
/// What the opt-out purge needs from a transport input.
#[derive(Clone)]
pub struct PurgeOptOutInput {
    pub diagnostics: Option<TelemetryDiagnostics>,
    pub source: Option<String>,
    pub spool_dir: PathBuf,
    pub spool_root: PathBuf,
}
impl From<&CreateOtlpTelemetryTransportInput> for PurgeOptOutInput {
    fn from(input: &CreateOtlpTelemetryTransportInput) -> Self {
        Self {
            diagnostics: input.diagnostics.clone(),
            source: input.source.clone(),
            spool_dir: input.spool_dir.clone(),
            spool_root: input.spool_root.clone(),
        }
    }
}
/// `PurgeOptOutInput` plus the liveness seams of `purge_idle_otlp_spool`.
#[derive(Clone)]
pub struct PurgeIdleOtlpSpoolInput {
    pub opt_out: PurgeOptOutInput,
    pub is_pid_alive: Option<IsPidAlive>,
    pub now_ms: Option<NowMs>,
}

/// Startup cleanup for a lazy adapter whose caller decided the persisted setting is off. The spool is
/// shared by every process of the product, so it is left alone while any writer is alive (a writer's
/// owner marker or active WAL file with a running pid, or an unexpired send claim): an idle writer whose
/// batches are all sealed still holds its marker, and that consented session sweeps its own spool when
/// its consent ends.
pub fn purge_idle_otlp_spool(input: &PurgeIdleOtlpSpoolInput) {
    let alive = |pid: u32| match &input.is_pid_alive {
        Some(f) => f(pid),
        None => is_pid_alive(pid),
    };
    let now = match &input.now_ms {
        Some(f) => f(),
        None => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64),
    };
    let spool_dir = &input.opt_out.spool_dir;
    let Ok(read) = fs::read_dir(spool_dir) else {
        return;
    };
    let owners = owners_dir(spool_dir);
    let names = read
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .chain(
            fs::read_dir(&owners)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned()),
        );
    let live = names.into_iter().any(|name| match parse_spool_name(&name) {
        SpoolEntry::Active { pid, .. } | SpoolEntry::Owner { pid, .. } => alive(pid),
        SpoolEntry::Sending {
            lease_expires_ms, ..
        } => lease_expires_ms > now,
        _ => false,
    });
    if live {
        return;
    }
    purge_on_opt_out(&input.opt_out);
    let mut owners_input = input.opt_out.clone();
    owners_input.spool_dir = owners;
    purge_on_opt_out(&owners_input);
}

/// The opt-out purge with the diagnostics sink wired to its report; every entry it leaves behind
/// becomes one diagnostic, by kind or errno code, never by path.
pub fn purge_owned_spool(
    dir: &Path,
    root: &Path,
    diagnostics: Option<&TelemetryDiagnostics>,
    source: &str,
) -> io::Result<PurgeOutcome> {
    let on_skipped =
        |kind: TelemetrySpoolEntryKind| purge_report(diagnostics, source, Some(kind), None);
    let on_failed = |code: &str| purge_report(diagnostics, source, None, Some(code));
    let report = PurgeReport {
        on_skipped: &on_skipped,
        on_failed: &on_failed,
    };
    purge_spool(dir, root, &report, &PurgeSeams::default())
}

pub fn purge_on_opt_out(input: &PurgeOptOutInput) {
    let source = input.source.clone().unwrap_or_else(|| "otlp".to_string());
    match purge_owned_spool(
        &input.spool_dir,
        &input.spool_root,
        input.diagnostics.as_ref(),
        &source,
    ) {
        Ok(PurgeOutcome::Refused) => {
            if let Some(diagnostics) = &input.diagnostics {
                diagnostics(&TelemetryDiagnosticInput {
                    event: TelemetryDiagnosticEvent::TelemetrySpoolPurgeRefused,
                    source,
                    error: None,
                    error_kind: None,
                    entry_kind: None,
                    error_code: None,
                    count: None,
                });
            }
        }
        Ok(_) => {}
        Err(error) => {
            if let Some(diagnostics) = &input.diagnostics {
                diagnostics(&TelemetryDiagnosticInput {
                    event: TelemetryDiagnosticEvent::TelemetrySpoolSweepFailed,
                    source,
                    error: Some(TelemetryError::new(error.to_string())),
                    error_kind: Some(TelemetryDiagnosticErrorKind::Error),
                    entry_kind: None,
                    error_code: None,
                    count: None,
                });
            }
        }
    }
}
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
    fn idle_input(root: &Path, alive: bool) -> PurgeIdleOtlpSpoolInput {
        PurgeIdleOtlpSpoolInput {
            opt_out: PurgeOptOutInput {
                diagnostics: None,
                source: None,
                spool_dir: root.join("spool"),
                spool_root: root.to_path_buf(),
            },
            is_pid_alive: Some(std::sync::Arc::new(move |_| alive)),
            now_ms: Some(std::sync::Arc::new(|| 1_000)),
        }
    }
    #[test]
    fn idle_purge_waits_for_a_live_owner_marker_then_removes_spool_and_owners() -> io::Result<()> {
        use crate::otlp_spool_names::{batch_name, owner_name};
        let root = tempfile::tempdir()?;
        let dir = root.path().join("spool");
        let owners = owners_dir(&dir);
        fs::create_dir(&dir)?;
        fs::create_dir(&owners)?;
        let batch = batch_name(Signal::Logs, "00000000-0000-7000-8000-000000000000");
        fs::write(dir.join(&batch), "x")?;
        fs::write(
            owners.join(owner_name(Signal::Logs, 7, &"b".repeat(32))),
            "",
        )?;
        purge_idle_otlp_spool(&idle_input(root.path(), true));
        assert!(dir.join(&batch).exists());
        purge_idle_otlp_spool(&idle_input(root.path(), false));
        assert!(!dir.exists());
        assert!(!owners.exists());
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
