//! The memory stack's only path to `std::fs`, and the two places where a plain call is not enough.
//!
//! Spec: `docs/cleanroom/specs/memory-fs-resilience.md`. Two things go wrong with direct filesystem calls on
//! a busy machine and on Windows:
//!
//! - A syscall can be interrupted by a signal and return `EINTR` with nothing done. Retrying is correct and
//!   the operation is safe to repeat.
//! - Windows refuses a rename while another handle holds the file. The holder usually lets go within
//!   milliseconds, so a short bounded ladder turns a spurious failure into a success.
//!
//! Both are bounded, and neither retries an operation whose partial progress makes a repeat unsafe.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

/// Upper bound on `EINTR` retries before the error is surfaced.
///
/// Bounded because a signal storm must not spin forever: if 128 retries all failed, the caller needs the error,
/// not a hang.
pub const EINTR_RETRY_CAP: usize = 128;

pub const CONTENTION_DELAYS_MS: [u64; 6] = [10, 25, 50, 100, 200, 400];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenamePlatform {
    Windows,
    Other,
}

impl RenamePlatform {
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

pub fn is_eintr(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::Interrupted
}

/// Runs `operation`, retrying while it fails with `EINTR`.
///
/// Only for operations that are safe to repeat. A multi-syscall write is not: it may have written half the
/// bytes before the interruption, and repeating it would duplicate that prefix.
pub fn retry_on_eintr<T>(mut operation: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut attempt = 0;
    loop {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) => {
                if !is_eintr(&error) || attempt >= EINTR_RETRY_CAP {
                    return Err(error);
                }
                attempt += 1;
            }
        }
    }
}

pub fn read(path: &Path) -> std::io::Result<Vec<u8>> {
    retry_on_eintr(|| std::fs::read(path))
}

pub fn read_to_string(path: &Path) -> std::io::Result<String> {
    retry_on_eintr(|| std::fs::read_to_string(path))
}

pub fn metadata(path: &Path) -> std::io::Result<std::fs::Metadata> {
    retry_on_eintr(|| std::fs::metadata(path))
}

pub fn symlink_metadata(path: &Path) -> std::io::Result<std::fs::Metadata> {
    retry_on_eintr(|| std::fs::symlink_metadata(path))
}

/// Whether an open flag asks for an exclusive create (`O_EXCL`), which must not be retried.
pub fn is_exclusive_flag(flag: &str) -> bool {
    flag.to_ascii_lowercase().contains('x')
}

/// Opens `path` for writing. An exclusive create gets exactly one attempt.
///
/// Retrying an exclusive create is wrong in a way that is easy to miss: the first attempt may have *succeeded*
/// in creating the file before being interrupted, and a retry would then fail with `AlreadyExists` — reporting
/// contention where the file is in fact ours.
pub fn open_with_exclusive_policy(path: &Path, flag: &str) -> std::io::Result<File> {
    let open_once = || open_options(flag).open(path);
    if is_exclusive_flag(flag) {
        open_once()
    } else {
        retry_on_eintr(open_once)
    }
}

fn open_options(flag: &str) -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    if flag.contains('a') {
        options.append(true).create(true);
    } else {
        options.truncate(true).create(true);
    }
    if is_exclusive_flag(flag) {
        options.create_new(true);
    }
    options
}

/// Writes every byte of `data`, retrying only the interrupted write.
///
/// `write_all` on a `File` already resumes from where it stopped rather than restarting the buffer, so this is
/// safe where a whole-call retry would not be.
pub fn write_all_to_handle(handle: &mut File, data: &[u8]) -> std::io::Result<usize> {
    retry_on_eintr(|| handle.write_all(data))?;
    Ok(data.len())
}

pub fn write_path_all(path: &Path, data: &[u8], flag: &str, flush: bool) -> std::io::Result<()> {
    let mut handle = open_with_exclusive_policy(path, flag)?;
    write_all_to_handle(&mut handle, data)?;
    if flush {
        retry_on_eintr(|| handle.sync_all())?;
    }
    Ok(())
}

/// Whether the error is a file-sharing contention code.
///
/// Windows reports a held handle as `ERROR_SHARING_VIOLATION` (32) or `ERROR_LOCK_VIOLATION` (33) rather than
/// as a POSIX `EBUSY`, so both families are recognised. On other platforms only the POSIX codes apply: 5 is
/// `EIO` there, not access-denied, and treating it as contention would retry a real I/O failure.
#[cfg(windows)]
pub fn is_contention_error(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::PermissionDenied
        || matches!(error.raw_os_error(), Some(1 | 5 | 13 | 16 | 32 | 33))
}

#[cfg(not(windows))]
pub fn is_contention_error(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(1 | 13 | 16))
}

/// The sleep is injected rather than performed, so the ladder is testable without waiting and the caller owns
/// the clock. On a platform other than Windows the ladder is empty: a POSIX rename is atomic and does not fail
/// this way.
pub fn rename_with_contention_retry(
    rename: &mut dyn FnMut(&Path, &Path) -> std::io::Result<()>,
    from: &Path,
    to: &Path,
    platform: RenamePlatform,
    delays_ms: &[u64],
    sleep_ms: &mut dyn FnMut(u64),
) -> std::io::Result<()> {
    let delays: &[u64] = if platform == RenamePlatform::Windows {
        delays_ms
    } else {
        &[]
    };
    let mut attempt = 0;
    loop {
        match rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) => {
                let Some(delay) = delays.get(attempt) else {
                    return Err(error);
                };
                if !is_contention_error(&error) {
                    return Err(error);
                }
                sleep_ms(*delay);
                attempt += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::io;

    fn interrupted() -> io::Error {
        io::Error::new(io::ErrorKind::Interrupted, "EINTR")
    }

    #[test]
    fn an_interrupted_call_is_retried() {
        let attempts = Cell::new(0);
        let value = retry_on_eintr(|| {
            attempts.set(attempts.get() + 1);
            if attempts.get() < 3 {
                Err(interrupted())
            } else {
                Ok(7)
            }
        })
        .expect("eventually succeeds");
        assert_eq!(value, 7);
        assert_eq!(attempts.get(), 3);
    }

    #[test]
    fn a_non_interrupted_error_is_surfaced_at_once() {
        let attempts = Cell::new(0);
        let error = retry_on_eintr(|| {
            attempts.set(attempts.get() + 1);
            Err::<(), _>(io::Error::new(io::ErrorKind::NotFound, "gone"))
        })
        .expect_err("surfaced");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(attempts.get(), 1);
    }

    #[test]
    fn retrying_is_bounded() {
        let attempts = Cell::new(0);
        let error = retry_on_eintr(|| {
            attempts.set(attempts.get() + 1);
            Err::<(), _>(interrupted())
        })
        .expect_err("gives up");
        assert!(is_eintr(&error));
        assert_eq!(attempts.get(), EINTR_RETRY_CAP + 1);
    }

    #[test]
    fn an_exclusive_flag_is_recognised() {
        assert!(is_exclusive_flag("wx"));
        assert!(is_exclusive_flag("WX"));
        assert!(is_exclusive_flag("ax"));
        assert!(!is_exclusive_flag("w"));
        assert!(!is_exclusive_flag("a"));
    }

    #[test]
    fn an_exclusive_create_is_not_retried() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a.txt");
        write_path_all(&path, b"first", "wx", false).expect("created");
        let error = write_path_all(&path, b"second", "wx", false).expect_err("refused");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(read_to_string(&path).expect("content"), "first");
    }

    #[test]
    fn a_plain_write_replaces_content() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a.txt");
        write_path_all(&path, b"first", "w", false).expect("written");
        write_path_all(&path, b"second", "w", false).expect("written");
        assert_eq!(read_to_string(&path).expect("content"), "second");
    }

    #[test]
    fn an_append_write_keeps_what_was_there() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a.txt");
        write_path_all(&path, b"one", "a", false).expect("written");
        write_path_all(&path, b"two", "a", false).expect("written");
        assert_eq!(read_to_string(&path).expect("content"), "onetwo");
    }

    #[test]
    fn a_flushed_write_lands_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a.txt");
        write_path_all(&path, b"durable", "w", true).expect("written");
        assert_eq!(read_to_string(&path).expect("content"), "durable");
    }

    #[test]
    fn reading_a_missing_file_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let error = read(&dir.path().join("gone")).expect_err("missing");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn symlink_metadata_does_not_follow_the_link() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target.txt");
        write_path_all(&target, b"x", "w", false).expect("written");
        let link = dir.path().join("link.txt");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        #[cfg(not(unix))]
        std::fs::copy(&target, &link).expect("copy");
        #[cfg(unix)]
        {
            assert!(symlink_metadata(&link).expect("stat").is_symlink());
            assert!(!metadata(&link).expect("stat").is_symlink());
        }
    }

    #[test]
    fn a_rename_that_succeeds_costs_no_delay() {
        let dir = tempfile::tempdir().expect("tempdir");
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        write_path_all(&from, b"x", "w", false).expect("written");
        let mut sleeps = 0;
        rename_with_contention_retry(
            &mut |from, to| std::fs::rename(from, to),
            &from,
            &to,
            RenamePlatform::Windows,
            &CONTENTION_DELAYS_MS,
            &mut |_| sleeps += 1,
        )
        .expect("renamed");
        assert_eq!(sleeps, 0);
    }

    #[test]
    fn a_contended_rename_walks_the_ladder() {
        let dir = tempfile::tempdir().expect("tempdir");
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        let attempts = Cell::new(0);
        let mut delays = Vec::new();
        rename_with_contention_retry(
            &mut |_, _| {
                attempts.set(attempts.get() + 1);
                if attempts.get() < 3 {
                    Err(io::Error::from_raw_os_error(16))
                } else {
                    Ok(())
                }
            },
            &from,
            &to,
            RenamePlatform::Windows,
            &CONTENTION_DELAYS_MS,
            &mut |delay| delays.push(delay),
        )
        .expect("eventually renamed");
        assert_eq!(attempts.get(), 3);
        assert_eq!(delays, vec![10, 25]);
    }

    #[test]
    fn the_ladder_ends_and_surfaces_the_last_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let attempts = Cell::new(0);
        let error = rename_with_contention_retry(
            &mut |_, _| {
                attempts.set(attempts.get() + 1);
                Err(io::Error::from_raw_os_error(16))
            },
            &dir.path().join("from"),
            &dir.path().join("to"),
            RenamePlatform::Windows,
            &CONTENTION_DELAYS_MS,
            &mut |_| {},
        )
        .expect_err("gives up");
        assert_eq!(error.raw_os_error(), Some(16));
        assert_eq!(attempts.get(), CONTENTION_DELAYS_MS.len() + 1);
    }

    #[test]
    fn a_non_contention_error_is_not_retried() {
        let dir = tempfile::tempdir().expect("tempdir");
        let attempts = Cell::new(0);
        let error = rename_with_contention_retry(
            &mut |_, _| {
                attempts.set(attempts.get() + 1);
                Err(io::Error::new(io::ErrorKind::NotFound, "gone"))
            },
            &dir.path().join("from"),
            &dir.path().join("to"),
            RenamePlatform::Windows,
            &CONTENTION_DELAYS_MS,
            &mut |_| {},
        )
        .expect_err("surfaced");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(attempts.get(), 1);
    }

    #[test]
    fn another_platform_has_no_ladder() {
        let dir = tempfile::tempdir().expect("tempdir");
        let attempts = Cell::new(0);
        let mut sleeps = 0;
        let error = rename_with_contention_retry(
            &mut |_, _| {
                attempts.set(attempts.get() + 1);
                Err(io::Error::from_raw_os_error(16))
            },
            &dir.path().join("from"),
            &dir.path().join("to"),
            RenamePlatform::Other,
            &CONTENTION_DELAYS_MS,
            &mut |_| sleeps += 1,
        )
        .expect_err("surfaced");
        assert_eq!(attempts.get(), 1);
        assert_eq!(sleeps, 0);
        let _ = error;
    }
}
