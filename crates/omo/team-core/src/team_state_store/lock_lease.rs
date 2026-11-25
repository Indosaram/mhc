//! Hand-over / restore lease for a held lock (`team-state-store/lock-lease.ts`, `#11262`).
//!
//! `hand_over(pid)` atomically replaces this process's record with a 5-line record naming `pid` and
//! a fresh instance id. `restore()` is compare-and-restore under `with_record_guard` for the
//! handed-over record so no reaper can remove it between the compare and the rename.

use std::cell::RefCell;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::lock_reclaim::{LockReclaimDeps, with_record_guard};
use super::lock_record::{is_path_absence_error, lock_process_start_line};
use crate::clock::now_ms;
use crate::error::{Result, TeamCoreError};

const LEASE_TEMP_SUFFIX: &str = ".lease";

/// Test seam callback invoked inside `with_record_guard` after the compare and before the rename.
pub type BeforeRestoreRenameFn<'a> = Box<dyn FnMut() -> io::Result<()> + 'a>;

#[derive(Default)]
pub struct LockLeaseOptions<'a> {
    pub stale_after_ms: Option<i64>,
    pub before_restore_rename: Option<BeforeRestoreRenameFn<'a>>,
}

pub struct LockLease<'a> {
    lock_path: PathBuf,
    owner_record: String,
    owner_tag: String,
    stale_after_ms: i64,
    before_restore_rename: RefCell<Option<BeforeRestoreRenameFn<'a>>>,
    handed_to: RefCell<Option<String>>,
}

impl<'a> LockLease<'a> {
    pub fn hand_over(&self, pid: u32) -> Result<()> {
        if self.handed_to.borrow().is_some() {
            return Err(TeamCoreError::message(
                "The lock is already handed over; restore it first.",
            ));
        }
        if pid == 0 || pid == std::process::id() {
            return Err(TeamCoreError::message(format!(
                "Cannot hand the lock to pid {pid}."
            )));
        }
        let observed = current_record(&self.lock_path)?;
        if observed.as_deref() != Some(self.owner_record.as_str()) {
            return Err(TeamCoreError::LockLeaseLost(
                self.lock_path.display().to_string(),
            ));
        }
        remove_leftover_temps(&self.lock_path)?;
        let process_start = lock_process_start_line(pid);
        let record = format!(
            "{} {}\n{pid}\n{}\n{}\n{process_start}\n",
            self.owner_tag,
            uuid::Uuid::new_v4(),
            now_ms(),
            uuid::Uuid::new_v4()
        );
        replace_record(&self.lock_path, &record)?;
        *self.handed_to.borrow_mut() = Some(record);
        Ok(())
    }

    pub fn restore(&self) -> Result<()> {
        let Some(expected) = self.handed_to.borrow_mut().take() else {
            return Err(TeamCoreError::message("The lock is not handed over."));
        };
        let mut guard_result: Option<Result<bool>> = None;
        let outcome = with_record_guard(
            &self.lock_path,
            &expected,
            self.stale_after_ms,
            &LockReclaimDeps::default(),
            || {
                let current = match current_record(&self.lock_path) {
                    Ok(value) => value,
                    Err(error) => {
                        guard_result = Some(Err(error.into()));
                        return Ok(false);
                    }
                };
                if current.as_deref() != Some(expected.as_str()) {
                    guard_result = Some(Ok(false));
                    return Ok(false);
                }
                if let Some(hook) = self.before_restore_rename.borrow_mut().as_mut()
                    && let Err(error) = hook()
                {
                    guard_result = Some(Err(error.into()));
                    return Ok(false);
                }
                if let Err(error) = replace_record(&self.lock_path, &self.owner_record) {
                    guard_result = Some(Err(error.into()));
                    return Ok(false);
                }
                guard_result = Some(Ok(true));
                Ok(true)
            },
        )?;
        if let Some(inner) = guard_result {
            if !inner? {
                return Err(TeamCoreError::LockLeaseLost(
                    self.lock_path.display().to_string(),
                ));
            }
            return Ok(());
        }
        if outcome != Some(true) {
            return Err(TeamCoreError::LockLeaseLost(
                self.lock_path.display().to_string(),
            ));
        }
        Ok(())
    }
}

#[must_use]
pub fn create_lock_lease<'a>(
    lock_path: impl Into<PathBuf>,
    owner_record: impl Into<String>,
    owner_tag: impl Into<String>,
    options: LockLeaseOptions<'a>,
) -> LockLease<'a> {
    LockLease {
        lock_path: lock_path.into(),
        owner_record: owner_record.into(),
        owner_tag: owner_tag.into(),
        stale_after_ms: options.stale_after_ms.unwrap_or(i64::MAX),
        before_restore_rename: RefCell::new(options.before_restore_rename),
        handed_to: RefCell::new(None),
    }
}

fn lease_temp_prefix(lock_path: &Path) -> String {
    let base = lock_path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    format!(".{base}.")
}

fn is_lease_temp_name(lock_path: &Path, entry: &str) -> bool {
    let prefix = lease_temp_prefix(lock_path);
    let Some(rest) = entry.strip_prefix(&prefix) else {
        return false;
    };
    let Some(uuid_part) = rest.strip_suffix(LEASE_TEMP_SUFFIX) else {
        return false;
    };
    is_lowercase_uuid(uuid_part)
}

fn is_lowercase_uuid(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes.iter().enumerate().all(|(idx, byte)| match idx {
        8 | 13 | 18 | 23 => *byte == b'-',
        _ => matches!(byte, b'0'..=b'9' | b'a'..=b'f'),
    })
}

fn replace_record(lock_path: &Path, content: &str) -> io::Result<()> {
    let parent = lock_path.parent().unwrap_or_else(|| Path::new("."));
    let temp_name = format!(
        "{}{}{}",
        lease_temp_prefix(lock_path),
        uuid::Uuid::new_v4(),
        LEASE_TEMP_SUFFIX
    );
    let temp_path = parent.join(temp_name);
    let write_result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp_path)?;
        file.write_all(content.as_bytes())?;
        drop(file);
        fs::rename(&temp_path, lock_path)
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    Ok(())
}

fn remove_leftover_temps(lock_path: &Path) -> io::Result<()> {
    let parent = lock_path.parent().unwrap_or_else(|| Path::new("."));
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if is_lease_temp_name(lock_path, &name_str) {
            let _ = fs::remove_file(entry.path());
        }
    }
    Ok(())
}

fn current_record(lock_path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(lock_path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if is_path_absence_error(&error) => Ok(None),
        Err(error) => Err(error),
    }
}
