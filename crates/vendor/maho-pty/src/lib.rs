mod session;
mod session_threads;
mod signals;

pub use session::{PtyError, PtyExit, PtyResult, PtySession, PtySessionOptions};

/// Native ABI version. This is INTENTIONALLY decoupled from the package/CalVer
/// version: it identifies the shape of the native surface (exports + signatures)
/// that `packages/pty/src/loader.ts` requires. Bump it ONLY on a
/// backward-incompatible change to that surface, and update `NATIVE_PTY_ABI_VERSION`
/// in the loader + the `__senpiPtyAbi<N>` sentinel export together. A CalVer
/// release must NOT change it — otherwise every release invalidates the prebuilt
/// binaries. See the sentinel export below.
const NATIVE_PTY_ABI_VERSION: &str = "1";

pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

// Stable ABI sentinel. The `js_name` MUST be a string literal (napi requirement),
// so it is spelled `__senpiPtyAbi<N>` where N == NATIVE_PTY_ABI_VERSION. The loader
// derives the same name from its own ABI constant and also checks the return value,
// rejecting any prebuilt binary compiled against a different ABI. This is decoupled
// from the CalVer package version on purpose so releases don't invalidate prebuilds.
pub fn senpi_pty_abi_sentinel() -> String {
    NATIVE_PTY_ABI_VERSION.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_crate_version() {
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn abi_sentinel_matches_abi_version() {
        assert_eq!(senpi_pty_abi_sentinel(), NATIVE_PTY_ABI_VERSION);
    }

    #[test]
    fn portable_pty_backend_is_linked() {
        let _pty_system = portable_pty::native_pty_system();
    }
}

#[cfg(test)]
mod session_tests;
