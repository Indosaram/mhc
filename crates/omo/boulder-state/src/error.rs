//! Failure modes of the boulder-state storage entry points.

use std::path::{Path, PathBuf};

/// Failure while reading, parsing, writing or removing a boulder state document.
///
/// Reads follow upstream `readBoulderState`: a missing, unreadable, unparsable or
/// non-object state file is reported as `Ok(None)` by [`read_boulder_state`], so the
/// `Read`, `InvalidJson` and `InvalidShape` variants are kept only for source
/// compatibility. Lifecycle entry points (`append_session_id*`, task upserts/timers,
/// `select_active_work`, `add_boulder_work`, `complete_boulder`) map a failed write to
/// `Ok(None)`; only [`write_boulder_state`] and [`clear_boulder_state`] still surface
/// `Write`/`Remove` (their boolean upstream form is `try_write_boulder_state` /
/// `try_clear_boulder_state`).
///
/// [`read_boulder_state`]: crate::read_boulder_state
/// [`write_boulder_state`]: crate::write_boulder_state
/// [`clear_boulder_state`]: crate::clear_boulder_state
#[derive(Debug)]
pub enum BoulderStateError {
    /// The state file exists but could not be read.
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The state file is not valid JSON.
    InvalidJson {
        path: PathBuf,
        source: serde_json::Error,
    },
    /// The state file is valid JSON but not a boulder document.
    InvalidShape { path: PathBuf, reason: &'static str },
    /// The state file (or its directory scaffold) could not be written.
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The state file could not be removed.
    Remove {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl BoulderStateError {
    /// Path of the file this error came from.
    pub fn path(&self) -> &Path {
        match self {
            Self::Read { path, .. }
            | Self::InvalidJson { path, .. }
            | Self::InvalidShape { path, .. }
            | Self::Write { path, .. }
            | Self::Remove { path, .. } => path,
        }
    }
}

impl std::fmt::Display for BoulderStateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(formatter, "failed to read {}: {source}", path.display())
            }
            Self::InvalidJson { path, source } => {
                write!(formatter, "{} is not valid JSON: {source}", path.display())
            }
            Self::InvalidShape { path, reason } => {
                write!(
                    formatter,
                    "{} is not boulder state: {reason}",
                    path.display()
                )
            }
            Self::Write { path, source } => {
                write!(formatter, "failed to write {}: {source}", path.display())
            }
            Self::Remove { path, source } => {
                write!(formatter, "failed to remove {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for BoulderStateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. }
            | Self::Write { source, .. }
            | Self::Remove { source, .. } => Some(source),
            Self::InvalidJson { source, .. } => Some(source),
            Self::InvalidShape { .. } => None,
        }
    }
}
