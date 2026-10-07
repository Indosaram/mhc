//! Reclamation of leaked `<lock>.candidate-<uuid>` files left by crashed lock contenders.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::fs;

/// Age after which an untracked candidate is treated as garbage (pin `candidate-sweep.ts`).
pub const CANDIDATE_STALE_AGE_MS: u64 = 60 * 60 * 1000;

/// Unlink attempts per candidate before a sharing error is reported (pin `candidate-sweep.ts`).
pub const CANDIDATE_UNLINK_ATTEMPTS: usize = 3;

static KNOWN_LEAKED_CANDIDATES: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

/// Records a candidate path this process published but has not yet removed.
pub fn track_leaked_candidate(candidate_path: &Path) {
    known_leaked()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_or_insert_with(HashSet::new)
        .insert(candidate_path.to_path_buf());
}

/// Forgets a candidate path once it is known to be gone.
pub fn forget_leaked_candidate(candidate_path: &Path) {
    if let Some(set) = known_leaked()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_mut()
    {
        set.remove(candidate_path);
    }
}

fn known_leaked() -> &'static Mutex<Option<HashSet<PathBuf>>> {
    &KNOWN_LEAKED_CANDIDATES
}

fn is_known_leaked(candidate_path: &Path) -> bool {
    known_leaked()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .is_some_and(|set| set.contains(candidate_path))
}

fn forget_candidates_in(lock_directory: &Path) {
    if let Some(set) = known_leaked()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_mut()
    {
        set.retain(|candidate| candidate.parent() != Some(lock_directory));
    }
}

/// Injectable seams for the sweeper.
#[derive(Default)]
pub struct CandidateSweepOptions<'a> {
    pub unlink: Option<&'a dyn Fn(&Path) -> std::io::Result<()>>,
    pub is_sharing_error: Option<&'a dyn Fn(&std::io::Error) -> bool>,
    pub on_failure: Option<&'a dyn Fn(&Path)>,
}

/// Removes leaked lock candidates, returning how many were reclaimed.
///
/// Age is the only liveness signal a candidate needs: nothing ever reads one after publish, so a
/// live lock or recovery file (which never contains `.candidate-<uuid>`) is never touched.
pub fn sweep_stale_lock_candidates(
    lock_directory: &Path,
    now_ms: impl Fn() -> u64,
    options: &CandidateSweepOptions<'_>,
) -> std::io::Result<usize> {
    let names = match fs::read_dir_names(lock_directory) {
        Ok(names) => names,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            forget_candidates_in(lock_directory);
            return Ok(0);
        }
        Err(error) => return Err(error),
    };

    let unlink: &dyn Fn(&Path) -> std::io::Result<()> = match options.unlink {
        Some(unlink) => unlink,
        None => &default_unlink,
    };
    let mut swept = 0usize;
    for name in names {
        if !is_leaked_candidate_name(&name) {
            continue;
        }
        let candidate_path = lock_directory.join(&name);
        let mut removed = false;
        let outcome = sweep_one(
            &candidate_path,
            &now_ms,
            options,
            unlink,
            &mut removed,
        );
        match outcome {
            Ok(()) => {
                if removed {
                    swept += 1;
                }
            }
            Err(error) => {
                if error.kind() == std::io::ErrorKind::NotFound {
                    forget_leaked_candidate(&candidate_path);
                }
                notify_failure(options, &candidate_path);
            }
        }
    }
    Ok(swept)
}

fn sweep_one(
    candidate_path: &Path,
    now_ms: &impl Fn() -> u64,
    options: &CandidateSweepOptions<'_>,
    unlink: &dyn Fn(&Path) -> std::io::Result<()>,
    removed: &mut bool,
) -> std::io::Result<()> {
    let metadata = fs::metadata(candidate_path)?;
    let mtime_ms = metadata
        .modified()
        .ok()
        .and_then(system_time_to_millis)
        .unwrap_or(0);
    if !is_known_leaked(candidate_path) && now_ms().saturating_sub(mtime_ms) <= CANDIDATE_STALE_AGE_MS
    {
        return Ok(());
    }

    for attempt in 0..CANDIDATE_UNLINK_ATTEMPTS {
        match unlink(candidate_path) {
            Ok(()) => {
                forget_leaked_candidate(candidate_path);
                *removed = true;
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                forget_leaked_candidate(candidate_path);
                *removed = true;
                return Ok(());
            }
            Err(error) => {
                let sharing = options
                    .is_sharing_error
                    .map(|probe| probe(&error))
                    .unwrap_or_else(|| default_is_sharing_error(&error));
                if !sharing {
                    return Err(error);
                }
                if attempt + 1 == CANDIDATE_UNLINK_ATTEMPTS {
                    notify_failure(options, candidate_path);
                }
            }
        }
    }
    Ok(())
}

fn default_unlink(path: &Path) -> std::io::Result<()> {
    fs::remove_file(path)
}

fn notify_failure(options: &CandidateSweepOptions<'_>, candidate_path: &Path) {
    if let Some(on_failure) = options.on_failure {
        on_failure(candidate_path);
    }
}

/// True when the error is a Windows sharing error; always false off Windows.
#[cfg(windows)]
pub fn default_is_sharing_error(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(5 | 32 | 33))
        || error.kind() == std::io::ErrorKind::PermissionDenied
}

/// True when the error is a Windows sharing error; always false off Windows.
#[cfg(not(windows))]
pub fn default_is_sharing_error(_error: &std::io::Error) -> bool {
    false
}

fn system_time_to_millis(time: std::time::SystemTime) -> Option<u64> {
    time.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
}

/// True only for the exact UUID-suffixed candidate shape `publishExclusive` generates.
pub fn is_leaked_candidate_name(name: &str) -> bool {
    let marker = ".candidate-";
    let Some(index) = name.rfind(marker) else {
        return false;
    };
    is_uuid_shape(&name[index + marker.len()..])
}

fn is_uuid_shape(value: &str) -> bool {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() != 36 {
        return false;
    }
    chars.iter().enumerate().all(|(index, ch)| {
        if matches!(index, 8 | 13 | 18 | 23) {
            *ch == '-'
        } else {
            ch.is_ascii_hexdigit()
        }
    })
}

#[cfg(test)]
#[path = "candidate_sweep_tests.rs"]
mod tests;
