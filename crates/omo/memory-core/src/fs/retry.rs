//! EINTR retry primitives for the memory filesystem boundary.

/// Upper bound on EINTR retries before the error is surfaced (pin `fs/retry.ts` `EINTR_RETRY_CAP`).
pub const EINTR_RETRY_CAP: usize = 128;

/// True when the error is a POSIX `EINTR` interruption.
pub fn is_eintr(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::Interrupted
}

/// Runs `operation`, retrying while it fails with `EINTR` up to [`EINTR_RETRY_CAP`] times.
pub fn retry_on_eintr<T>(mut operation: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut attempt = 0usize;
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

#[cfg(test)]
#[path = "retry_tests.rs"]
mod tests;
