use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use super::lock_record::{mark_lock_record_held, mark_lock_record_released};
pub use super::locks::{LockOpenErrorDeps, assert_retryable_lock_open_error, assert_retryable_lock_open_error_with};

pub fn publish_lock_record(lock_path: &Path, record: &str) -> io::Result<bool> {
    mark_lock_record_held(record);
    let mut file = match OpenOptions::new().write(true).create_new(true).open(lock_path) {
        Ok(file) => file,
        Err(error) => {
            mark_lock_record_released(record);
            assert_retryable_lock_open_error(lock_path, error)?;
            return Ok(false);
        }
    };
    if let Err(error) = file.write_all(record.as_bytes()) {
        mark_lock_record_released(record);
        drop(file);
        // Upstream deliberately ignores rollback unlink failure and preserves the write error.
        let _ = fs::remove_file(lock_path);
        return Err(error);
    }
    drop(file);
    Ok(true)
}
