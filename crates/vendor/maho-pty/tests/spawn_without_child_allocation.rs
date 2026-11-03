//! The PTY child runs pre-exec code between fork and exec. In a multi-threaded parent another
//! thread can hold an allocator lock at fork time, and the child inherits that lock held forever,
//! so any allocation there hangs the child and the parent's `Command::spawn` with it. This binary
//! models that state deterministically: once armed, an allocation in any process other than the
//! test process exits it with `ALLOCATED_IN_CHILD` instead of hanging.
#![cfg(unix)]

use maho_pty::{PtySession, PtySessionOptions};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

const ALLOCATED_IN_CHILD: i32 = 97;

/// pid of the test process once armed; 0 while disarmed.
static PARENT_PID: AtomicI32 = AtomicI32::new(0);

struct ChildAllocationTrap;

impl ChildAllocationTrap {
    fn check() {
        let parent = PARENT_PID.load(Ordering::Relaxed);
        // SAFETY: getpid and _exit are async-signal-safe and touch no allocator state.
        if parent != 0 && unsafe { libc::getpid() } != parent {
            unsafe { libc::_exit(ALLOCATED_IN_CHILD) };
        }
    }
}

// SAFETY: forwards every call to the system allocator unchanged after the check.
unsafe impl GlobalAlloc for ChildAllocationTrap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::check();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::check();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: ChildAllocationTrap = ChildAllocationTrap;

#[test]
fn pty_child_reaches_exec_without_allocating() {
    let output = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&output);
    PARENT_PID.store(std::process::id() as i32, Ordering::Relaxed);
    let mut session = PtySession::start(
        PtySessionOptions::new("sh").arg("-c").arg("printf exec-reached"),
        move |chunk| seen.lock().unwrap().extend_from_slice(chunk),
    )
    .unwrap();
    let exit = session.wait().unwrap();
    PARENT_PID.store(0, Ordering::Relaxed);

    assert_eq!(
        exit.exit_code,
        Some(0),
        "the child allocated between fork and exec (exit {ALLOCATED_IN_CHILD} means it did)"
    );
    assert_eq!(String::from_utf8_lossy(&output.lock().unwrap()), "exec-reached");
}
