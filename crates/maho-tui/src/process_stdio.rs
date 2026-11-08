//! Process-wide stdout/stderr write seam.
//!
//! senpi monkeypatches `process.stdout.write` / `process.stderr.write` (external-stdout guard,
//! stderr observer). Rust cannot patch `std::io::stdout`, so crate code and hosts write through
//! [`stdout_write`] / [`stderr_write`], whose current writer can be swapped like the JS property.

use std::io::Write;
use std::sync::{Arc, Mutex};

pub type StreamWrite = Arc<dyn Fn(&str) + Send + Sync>;

fn default_stdout() -> StreamWrite {
    Arc::new(|text: &str| {
        // The buffer is kept for the life of the process and flushed at the end of every call, so
        // output ordering is unchanged. A large frame then reaches the terminal as one
        // `write_all` instead of the per-call handle lock plus one syscall per stdio write.
        static STDOUT: Mutex<Option<std::io::BufWriter<std::io::Stdout>>> = Mutex::new(None);
        let mut guard = STDOUT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let writer = guard
            .get_or_insert_with(|| std::io::BufWriter::with_capacity(64 * 1024, std::io::stdout()));
        let _ = writer.write_all(text.as_bytes());
        let _ = writer.flush();
    })
}

fn default_stderr() -> StreamWrite {
    Arc::new(|text: &str| {
        static STDERR: Mutex<Option<std::io::BufWriter<std::io::Stderr>>> = Mutex::new(None);
        let mut guard = STDERR
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let writer = guard
            .get_or_insert_with(|| std::io::BufWriter::with_capacity(16 * 1024, std::io::stderr()));
        let _ = writer.write_all(text.as_bytes());
        let _ = writer.flush();
    })
}

/// Serializes tests that swap the process-wide writers.
#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

static STDOUT: Mutex<Option<StreamWrite>> = Mutex::new(None);
static STDERR: Mutex<Option<StreamWrite>> = Mutex::new(None);

fn current(slot: &Mutex<Option<StreamWrite>>, default: fn() -> StreamWrite) -> StreamWrite {
    let mut guard = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(guard.get_or_insert_with(default))
}

fn replace(
    slot: &Mutex<Option<StreamWrite>>,
    default: fn() -> StreamWrite,
    next: StreamWrite,
) -> StreamWrite {
    let mut guard = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let previous = guard.take().unwrap_or_else(default);
    *guard = Some(next);
    previous
}

/// `process.stdout.write` (the currently installed writer).
pub fn stdout_writer() -> StreamWrite {
    current(&STDOUT, default_stdout)
}

/// Assigns `process.stdout.write`; returns the previous writer.
pub fn set_stdout_writer(next: StreamWrite) -> StreamWrite {
    replace(&STDOUT, default_stdout, next)
}

pub fn stdout_write(text: &str) {
    stdout_writer()(text);
}

/// `console.log`: writes the text plus a newline to stdout.
pub fn console_log(text: &str) {
    stdout_write(&format!("{text}\n"));
}

pub fn stderr_writer() -> StreamWrite {
    current(&STDERR, default_stderr)
}

pub fn set_stderr_writer(next: StreamWrite) -> StreamWrite {
    replace(&STDERR, default_stderr, next)
}

pub fn stderr_write(text: &str) {
    stderr_writer()(text);
}
