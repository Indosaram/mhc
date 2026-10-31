//! Identity-scoped needs-human watermark, modeled on `soul/watermark.rs`.
//!
//! The file records which agreements were already announced as needs-human; each newly flagged
//! item is returned exactly once, and an item the person acknowledged (status back to active) leaves
//! the watermark so a later re-flag is announced again. Read, decision, and replacement run under
//! the identity's notice lock.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::git::GitMemoryRepo;
use crate::journal::store::{JournalError, sync_journal_directory};
use crate::locks::acquire::{AcquireLockError, AcquireLockOptions, WithLockError, with_lock};
use crate::locks::domains::notice_lock_path;
use crate::locks::lock_record::{CreateLockRecordOptions, create_lock_record};

use super::ledger_file::{agreement_items_of, read_agreements_at};
use super::types::{AgreementItem, AgreementStatus};

pub const AGREEMENTS_NOTICE_WATERMARK_FILENAME: &str = "agreements-flagged.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumeAgreementsNoticeOptions {
    pub notices_dir: PathBuf,
    pub locks_dir: PathBuf,
    pub wait_timeout_ms: Option<u64>,
}

#[derive(Debug)]
pub enum AgreementsNoticeError {
    Lock(String),
    Io(std::io::Error),
    Git(String),
}

impl fmt::Display for AgreementsNoticeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lock(message) => {
                write!(formatter, "lock error in agreements watermark: {message}")
            }
            Self::Io(error) => write!(formatter, "io error in agreements watermark: {error}"),
            Self::Git(message) => write!(formatter, "git error in agreements watermark: {message}"),
        }
    }
}

impl std::error::Error for AgreementsNoticeError {}

impl From<std::io::Error> for AgreementsNoticeError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Receives non-fatal warnings (message, detail); the default sink drops them so a host TUI is
/// never written to.
pub type AgreementsWarnSink<'a> = &'a dyn Fn(&str, Option<&str>);

/// Returns the needs-human items at HEAD that were not announced yet, then records them as announced.
pub fn consume_agreements_notice_delta(
    repo: &GitMemoryRepo,
    options: &ConsumeAgreementsNoticeOptions,
) -> Result<Vec<AgreementItem>, AgreementsNoticeError> {
    consume_agreements_notice_delta_with_warn(repo, options, &|_, _| {})
}

/// [`consume_agreements_notice_delta`] with the upstream `onWarn` sink.
pub fn consume_agreements_notice_delta_with_warn(
    repo: &GitMemoryRepo,
    options: &ConsumeAgreementsNoticeOptions,
    on_warn: AgreementsWarnSink<'_>,
) -> Result<Vec<AgreementItem>, AgreementsNoticeError> {
    let head = repo
        .head()
        .map_err(|error| AgreementsNoticeError::Git(error.to_string()))?;
    if let Some(head) = head {
        let content = read_agreements_at(repo, &head)
            .map_err(|error| AgreementsNoticeError::Git(error.to_string()))?;
        let flagged: BTreeSet<String> = agreement_items_of(content.as_deref())
            .into_iter()
            .filter(|item| item.status == AgreementStatus::NeedsHuman)
            .map(|item| item.id)
            .collect();
        // An anomalous optimistic read is retried under the lock, which preserves its errors.
        // A differing set, including an acknowledgement, must be re-read under the lock first.
        if let Ok(Some(announced)) = read_watermark(&options.notices_dir)
            && announced == flagged
        {
            return Ok(Vec::new());
        }
    }
    let record = create_lock_record(
        "agreements notice watermark",
        CreateLockRecordOptions::default(),
    )
    .map_err(|error| AgreementsNoticeError::Lock(error.to_string()))?;
    let acquire = AcquireLockOptions {
        wait_timeout_ms: Some(options.wait_timeout_ms.unwrap_or(2_000)),
        ..Default::default()
    };
    let result = with_lock(
        &notice_lock_path(&options.locks_dir),
        &record,
        &acquire,
        || consume_under_lock(repo, &options.notices_dir, on_warn),
    );
    match result {
        Ok(items) => Ok(items),
        Err(WithLockError::Acquire(AcquireLockError::Contention(_))) => Ok(Vec::new()),
        Err(WithLockError::Acquire(error)) => Err(AgreementsNoticeError::Lock(error.to_string())),
        Err(WithLockError::User(error)) => Err(error),
    }
}

fn consume_under_lock(
    repo: &GitMemoryRepo,
    notices_dir: &Path,
    on_warn: AgreementsWarnSink<'_>,
) -> Result<Vec<AgreementItem>, AgreementsNoticeError> {
    let head = repo
        .head()
        .map_err(|error| AgreementsNoticeError::Git(error.to_string()))?;
    let Some(head) = head else {
        return Ok(Vec::new());
    };
    let content = read_agreements_at(repo, &head)
        .map_err(|error| AgreementsNoticeError::Git(error.to_string()))?;
    let flagged: Vec<AgreementItem> = agreement_items_of(content.as_deref())
        .into_iter()
        .filter(|item| item.status == AgreementStatus::NeedsHuman)
        .collect();
    let announced = read_watermark(notices_dir)?;
    let fresh: Vec<AgreementItem> = flagged
        .iter()
        .filter(|item| !announced.as_ref().is_some_and(|set| set.contains(&item.id)))
        .cloned()
        .collect();
    let next: Vec<String> = flagged
        .iter()
        .map(|item| item.id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if !fresh.is_empty() || Some(next.len()) != announced.as_ref().map(BTreeSet::len) {
        write_watermark(notices_dir, &next, on_warn)?;
    }
    Ok(fresh)
}

/// A missing or corrupt watermark reads as `None` (nothing announced yet).
fn read_watermark(notices_dir: &Path) -> Result<Option<BTreeSet<String>>, AgreementsNoticeError> {
    let path = notices_dir.join(AGREEMENTS_NOTICE_WATERMARK_FILENAME);
    let raw = match crate::fs::resilient::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AgreementsNoticeError::Io(error)),
    };
    let Ok(Value::Object(parsed)) = serde_json::from_str::<Value>(&raw) else {
        return Ok(None);
    };
    if parsed.get("version").and_then(Value::as_u64) != Some(1) {
        return Ok(None);
    }
    let Some(Value::Array(ids)) = parsed.get("announced") else {
        return Ok(None);
    };
    let announced: Option<BTreeSet<String>> = ids
        .iter()
        .map(|id| id.as_str().map(str::to_string))
        .collect();
    Ok(announced)
}

fn write_watermark(
    notices_dir: &Path,
    announced: &[String],
    on_warn: AgreementsWarnSink<'_>,
) -> Result<(), AgreementsNoticeError> {
    create_private_dir(notices_dir)?;
    let target = notices_dir.join(AGREEMENTS_NOTICE_WATERMARK_FILENAME);
    let temporary = notices_dir.join(format!(
        "{AGREEMENTS_NOTICE_WATERMARK_FILENAME}.tmp-{}-{}",
        std::process::id(),
        crate::support::random::random_id()
    ));
    let json = serde_json::json!({ "version": 1, "announced": announced });
    let published = write_private_file(&temporary, format!("{json}\n").as_bytes())
        .and_then(|()| crate::fs::resilient::rename(&temporary, &target));
    if let Err(error) = published {
        // Cleanup is best-effort: a failing remove must not replace the original error.
        let _ = crate::fs::resilient::remove_file(&temporary);
        return Err(error.into());
    }
    if let Err(JournalError::Io(error)) = sync_journal_directory(notices_dir) {
        // The rename already consumed this notice; a durability failure must not discard it.
        on_warn(
            "[memory] Published agreements watermark directory sync failed:",
            Some(&crate::support::errno::io_error_code(&error)),
        );
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    crate::fs::resilient::create_dir_all(dir)
}

#[cfg(unix)]
fn write_private_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(data)?;
    file.sync_all()
}

#[cfg(not(unix))]
fn write_private_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(data)?;
    file.sync_all()
}

#[cfg(test)]
#[path = "notice_watermark_tests.rs"]
mod tests;
