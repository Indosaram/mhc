//! Keeps this process's std handles out of the processes it spawns unless a spawn asks for them.
//!
//! Unix gets this from `O_CLOEXEC`: a child only receives the descriptors its `Stdio` names. Windows
//! does not: std's `CreateProcessW` passes `bInheritHandles = TRUE` without a handle list, so a child
//! inherits EVERY inheritable handle its parent holds, and std handles a process was given as its
//! parent's pipe ends are inheritable. Left alone, every descendant - whatever `Stdio` it was given,
//! a detached daemon included - keeps a copy of those pipe ends, so whoever reads this process's
//! stdout sees no end-of-file after it exits until the last such descendant exits too.
//!
//! Clearing the inherit flag on the originals changes nothing a spawn asks for: `Stdio::inherit()`
//! duplicates the std handle as a fresh inheritable handle for exactly the child being spawned.

/// Clears the inherit flag on this process's stdin/stdout/stderr handles (a no-op off Windows, where
/// `O_CLOEXEC` already gives this). Every handle is attempted; the first failure is returned.
#[cfg(windows)]
pub fn keep_std_handles_private() -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
    unsafe extern "system" {
        fn SetHandleInformation(handle: *mut std::ffi::c_void, mask: u32, flags: u32) -> i32;
    }
    let mut first_error = None;
    for handle in [std::io::stdin().as_raw_handle(), std::io::stdout().as_raw_handle(), std::io::stderr().as_raw_handle()] {
        // Null: launched without that std handle (a detached parent); nothing to keep private.
        if handle.is_null() {
            continue;
        }
        // SAFETY: `handle` is this process's own live std handle (std maps an absent one to null,
        // skipped above); only its inherit flag changes, nothing is closed or replaced.
        if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 && first_error.is_none() {
            first_error = Some(std::io::Error::last_os_error());
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Clears the inherit flag on this process's std handles: already true off Windows (`O_CLOEXEC`).
#[cfg(not(windows))]
pub fn keep_std_handles_private() -> std::io::Result<()> {
    Ok(())
}
