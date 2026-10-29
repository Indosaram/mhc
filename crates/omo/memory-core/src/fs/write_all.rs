//! Stateful write paths: whole-call retry is unsafe for multi-syscall writes.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use super::retry::retry_on_eintr;

/// True when an open flag requests an exclusive create (pin `write-all.ts` `isExclusiveFlag`).
pub fn is_exclusive_flag(flag: &str) -> bool {
    flag.to_ascii_lowercase().contains('x')
}

/// Opens `path` for writing; an exclusive flag gets exactly one attempt, a shareable flag retries.
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

/// Writes every byte of `data`, retrying interrupted writes; returns the bytes written.
pub fn write_all_to_handle(handle: &mut File, data: &[u8]) -> std::io::Result<usize> {
    retry_on_eintr(|| handle.write_all(data))?;
    Ok(data.len())
}

/// Opens `path` with `flag`, writes all of `data`, optionally flushes, and closes the handle.
pub fn write_path_all(path: &Path, data: &[u8], flag: &str, flush: bool) -> std::io::Result<()> {
    let mut handle = open_with_exclusive_policy(path, flag)?;
    write_all_to_handle(&mut handle, data)?;
    if flush {
        retry_on_eintr(|| handle.sync_all())?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "write_all_tests.rs"]
mod tests;
