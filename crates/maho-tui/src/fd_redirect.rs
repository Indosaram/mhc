//! Port of senpi `packages/coding-agent/src/modes/interactive/stderr-fd-redirect.ts` (#2284) and
//! `stdout-fd-redirect.ts` (#2815), pin 903b8336.
//!
//! Unix only, matching upstream: `loadStderrFdSyscalls` returns `undefined` on Windows and outside
//! Bun, so senpi's own fd redirect is a no-op there and the JS-level guard stays in force. This
//! module is therefore `#[cfg(unix)]` in [`crate::lib`]; on Windows the terminal size is read with
//! `crossterm::terminal::size`, because stdout is never re-pointed at a log there.
//!
//! The process-wide writer seam in [`crate::process_stdio`] only sees writes that go through it.
//! A child spawned with inherited stdio, or any code that writes fd 1 or fd 2 directly, draws over
//! the terminal the TUI owns. While the TUI owns the screen, the descriptor is pointed at the debug
//! log with `dup2`, and the original descriptor is put back before the terminal is released.
//!
//! For stdout, the renderer's own writer ([`crate::process_stdio::stdout_write`]) is re-pointed at
//! a duplicate of the terminal descriptor, so its writes and window-size reads keep reaching the
//! terminal. senpi loads `dup`/`dup2` through `bun:ffi`. Here they come from `rustix`, and the
//! injectable `StderrFdSyscalls` becomes the [`StdioFdSlot`] trait.

use std::fs::OpenOptions;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::process_stdio::{self, StreamWrite};

/// One standard descriptor that can be duplicated and re-pointed (senpi `StderrFdSyscalls`).
pub trait StdioFdSlot {
    /// `dup(fd)`: a new descriptor for whatever the slot points at now.
    fn dup(&self) -> rustix::io::Result<OwnedFd>;
    /// `dup2(source, fd)`: point the slot at `source`.
    fn point_at(&self, source: BorrowedFd<'_>) -> rustix::io::Result<()>;
}

/// fd 2.
pub struct StderrSlot;

impl StdioFdSlot for StderrSlot {
    fn dup(&self) -> rustix::io::Result<OwnedFd> {
        rustix::io::dup(rustix::stdio::stderr())
    }
    fn point_at(&self, source: BorrowedFd<'_>) -> rustix::io::Result<()> {
        rustix::stdio::dup2_stderr(source)
    }
}

/// fd 1.
pub struct StdoutSlot;

impl StdioFdSlot for StdoutSlot {
    fn dup(&self) -> rustix::io::Result<OwnedFd> {
        rustix::io::dup(rustix::stdio::stdout())
    }
    fn point_at(&self, source: BorrowedFd<'_>) -> rustix::io::Result<()> {
        rustix::stdio::dup2_stdout(source)
    }
}

fn open_log(log_path: &Path) -> Option<OwnedFd> {
    OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(log_path)
        .ok()
        .map(OwnedFd::from)
}

/// senpi `StderrFdRedirect`: puts the original descriptor back once.
pub struct StderrFdRedirect<S: StdioFdSlot> {
    slot: S,
    saved: Option<OwnedFd>,
}

impl<S: StdioFdSlot> StderrFdRedirect<S> {
    pub fn restore(&mut self) {
        if let Some(saved) = self.saved.take() {
            let _ = self.slot.point_at(saved.as_fd());
        }
    }
}

/// senpi `redirectStderrFd`, over any slot: point it at `log_path` (append, 0600). Returns `None`,
/// leaving the slot untouched, when any step fails.
pub fn redirect_fd<S: StdioFdSlot>(slot: S, log_path: &Path) -> Option<StderrFdRedirect<S>> {
    let saved = slot.dup().ok()?;
    let target = open_log(log_path)?;
    slot.point_at(target.as_fd()).ok()?;
    Some(StderrFdRedirect {
        slot,
        saved: Some(saved),
    })
}

/// senpi `redirectStderrFd`: point fd 2 at `log_path`.
pub fn redirect_stderr_fd(log_path: &Path) -> Option<StderrFdRedirect<StderrSlot>> {
    redirect_fd(StderrSlot, log_path)
}

fn write_all(fd: BorrowedFd<'_>, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        match rustix::io::write(fd, bytes) {
            Ok(0) | Err(_) => return,
            Ok(written) => bytes = &bytes[written..],
        }
    }
}

/// senpi `StdoutFdRedirect`.
pub struct StdoutFdRedirect<S: StdioFdSlot> {
    slot: S,
    terminal: Arc<OwnedFd>,
    active: Arc<AtomicBool>,
    writer: StreamWrite,
    previous: StreamWrite,
}

impl<S: StdioFdSlot> StdoutFdRedirect<S> {
    pub fn restore(&mut self) {
        if !self.active.swap(false, Ordering::SeqCst) {
            return;
        }
        let _ = self.slot.point_at(self.terminal.as_fd());
        // A writer installed on top of ours keeps calling ours, which now forwards to `previous`.
        if Arc::ptr_eq(&process_stdio::stdout_writer(), &self.writer) {
            process_stdio::set_stdout_writer(Arc::clone(&self.previous));
        }
        let mut active = ACTIVE_TERMINAL
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if active
            .as_ref()
            .is_some_and(|fd| Arc::ptr_eq(fd, &self.terminal))
        {
            *active = None;
        }
    }
}

/// The terminal descriptor behind the active stdout redirect, for window-size reads.
static ACTIVE_TERMINAL: Mutex<Option<Arc<OwnedFd>>> = Mutex::new(None);

/// senpi's `columns`/`rows` override: while fd 1 is the log, the size comes from the terminal.
pub fn redirected_terminal_size() -> Option<(u16, u16)> {
    let active = ACTIVE_TERMINAL
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let size = rustix::termios::tcgetwinsize(active.as_ref()?.as_fd()).ok()?;
    Some((size.ws_col, size.ws_row))
}

/// senpi `redirectStdoutFd`, over any slot with a given terminal duplicate: point the slot at
/// `log_path` and re-point the [`process_stdio`] stdout writer at `terminal`. Returns `None`,
/// leaving the slot and the writer untouched, when any step fails.
pub fn redirect_stdout_with<S: StdioFdSlot>(
    slot: S,
    terminal: Arc<OwnedFd>,
    log_path: &Path,
) -> Option<StdoutFdRedirect<S>> {
    let target = open_log(log_path)?;
    slot.point_at(target.as_fd()).ok()?;
    let active = Arc::new(AtomicBool::new(true));
    let previous_slot: Arc<Mutex<Option<StreamWrite>>> = Arc::new(Mutex::new(None));
    let writer: StreamWrite = {
        let terminal = Arc::clone(&terminal);
        let active = Arc::clone(&active);
        let previous_slot = Arc::clone(&previous_slot);
        Arc::new(move |text: &str| {
            if active.load(Ordering::SeqCst) {
                write_all(terminal.as_fd(), text.as_bytes());
                return;
            }
            let previous = previous_slot
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if let Some(previous) = previous {
                previous(text);
            }
        })
    };
    let previous = process_stdio::set_stdout_writer(Arc::clone(&writer));
    *previous_slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&previous));
    Some(StdoutFdRedirect {
        slot,
        terminal,
        active,
        writer,
        previous,
    })
}

// One duplicate of the terminal per process: every takeover after a suspend or an external editor
// reuses it, so a long session does not accumulate descriptors.
static TERMINAL: Mutex<Option<Arc<OwnedFd>>> = Mutex::new(None);

/// senpi `redirectStdoutFd`: point fd 1 at `log_path` and keep the renderer on the terminal.
pub fn redirect_stdout_fd(log_path: &Path) -> Option<StdoutFdRedirect<StdoutSlot>> {
    let terminal = {
        let mut cached = TERMINAL.lock().unwrap_or_else(PoisonError::into_inner);
        match cached.as_ref() {
            Some(terminal) => Arc::clone(terminal),
            None => {
                let terminal = Arc::new(StdoutSlot.dup().ok()?);
                *cached = Some(Arc::clone(&terminal));
                terminal
            }
        }
    };
    let redirect = redirect_stdout_with(StdoutSlot, Arc::clone(&terminal), log_path)?;
    *ACTIVE_TERMINAL
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(terminal);
    Some(redirect)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A slot over a private descriptor, standing in for fd 1 or fd 2 so the test process's real
    /// descriptors are never touched.
    struct FileSlot(RefCell<OwnedFd>);

    impl StdioFdSlot for &FileSlot {
        fn dup(&self) -> rustix::io::Result<OwnedFd> {
            rustix::io::dup(&*self.0.borrow())
        }
        fn point_at(&self, source: BorrowedFd<'_>) -> rustix::io::Result<()> {
            rustix::io::dup2(source, &mut self.0.borrow_mut())
        }
    }

    fn slot_over(path: &Path) -> FileSlot {
        FileSlot(RefCell::new(
            open_log(path).expect("open terminal stand-in"),
        ))
    }

    fn write_slot(slot: &FileSlot, text: &str) {
        write_all(slot.0.borrow().as_fd(), text.as_bytes());
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn redirects_into_the_log_and_restores_the_original_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (terminal, log) = (dir.path().join("terminal"), dir.path().join("debug.log"));
        let slot = slot_over(&terminal);
        let mut redirect = redirect_fd(&slot, &log).expect("redirect");
        write_slot(&slot, "hidden\n");
        redirect.restore();
        redirect.restore();
        write_slot(&slot, "visible\n");
        assert_eq!(read(&log), "hidden\n");
        assert_eq!(read(&terminal), "visible\n");
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&log)
            .expect("log metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn leaves_the_descriptor_untouched_when_the_log_cannot_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let terminal = dir.path().join("terminal");
        let slot = slot_over(&terminal);
        assert!(redirect_fd(&slot, &dir.path().join("missing").join("debug.log")).is_none());
        write_slot(&slot, "still visible\n");
        assert_eq!(read(&terminal), "still visible\n");
    }

    #[test]
    fn stdout_redirect_keeps_the_renderer_on_the_terminal() {
        let _lock = process_stdio::TEST_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let dir = tempfile::tempdir().expect("tempdir");
        let (terminal_path, log) = (dir.path().join("terminal"), dir.path().join("debug.log"));
        let slot = slot_over(&terminal_path);
        let terminal = Arc::new((&slot).dup().expect("terminal duplicate"));
        let captured = Arc::new(Mutex::new(String::new()));
        let original: StreamWrite = {
            let captured = Arc::clone(&captured);
            Arc::new(move |text: &str| captured.lock().expect("captured").push_str(text))
        };
        let before = process_stdio::set_stdout_writer(Arc::clone(&original));

        let mut redirect = redirect_stdout_with(&slot, terminal, &log).expect("redirect");
        process_stdio::stdout_write("frame\n");
        write_slot(&slot, "stray child output\n");
        // A guard wrapped on top of ours stays installed past restore and must reach the original.
        let ours = process_stdio::stdout_writer();
        let wrapper: StreamWrite = Arc::new(move |text: &str| ours(text));
        process_stdio::set_stdout_writer(Arc::clone(&wrapper));
        redirect.restore();
        process_stdio::stdout_write("after\n");
        write_slot(&slot, "restored\n");
        process_stdio::set_stdout_writer(before);

        assert_eq!(read(&terminal_path), "frame\nrestored\n");
        assert_eq!(read(&log), "stray child output\n");
        assert_eq!(*captured.lock().expect("captured"), "after\n");
    }

    #[test]
    fn stdout_restore_reinstates_the_previous_writer() {
        let _lock = process_stdio::TEST_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let dir = tempfile::tempdir().expect("tempdir");
        let slot = slot_over(&dir.path().join("terminal"));
        let terminal = Arc::new((&slot).dup().expect("terminal duplicate"));
        let original: StreamWrite = Arc::new(|_: &str| {});
        let before = process_stdio::set_stdout_writer(Arc::clone(&original));
        let mut redirect =
            redirect_stdout_with(&slot, terminal, &dir.path().join("debug.log")).expect("redirect");
        assert!(!Arc::ptr_eq(&process_stdio::stdout_writer(), &original));
        redirect.restore();
        assert!(Arc::ptr_eq(&process_stdio::stdout_writer(), &original));
        process_stdio::set_stdout_writer(before);
    }
}
