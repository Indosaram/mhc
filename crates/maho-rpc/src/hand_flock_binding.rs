//! Port of senpi `packages/coding-agent/src/modes/rpc/hand-flock-binding.ts`.
//!
//! In Rust, `std::fs::File::try_lock_shared` wraps the platform's shared non-blocking advisory lock
//! (`flock(LOCK_SH | LOCK_NB)` on Unix, `LockFileEx` on Windows) without `unsafe` FFI.

use std::sync::OnceLock;

use crate::hand_activity_lock::FlockBackend;

static BOUND: OnceLock<Option<FlockBackend>> = OnceLock::new();

/// Returns the platform's advisory file-lock backend (`FlockBackend::Std` on Unix/Windows), or
/// `None` if unsupported.
pub fn load_flock() -> Option<FlockBackend> {
    BOUND.get_or_init(bind_flock).clone()
}

fn bind_flock() -> Option<FlockBackend> {
    if cfg!(any(target_os = "macos", target_os = "linux", windows)) {
        Some(FlockBackend::Std)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_flock_on_supported_platforms() {
        let loaded = load_flock();
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            assert!(loaded.is_some());
        }
    }
}
