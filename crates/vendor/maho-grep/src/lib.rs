mod cancel;
mod matcher;
mod options;
mod search;
mod walk;

pub use cancel::CancelToken;
pub use options::{
    GrepCounts, GrepError, GrepFileCount, GrepMatch, GrepMode, GrepOptions, GrepResult, GrepWarning,
};
pub use search::search;

/// Native ABI version. This is INTENTIONALLY decoupled from the package/CalVer
/// version: it identifies the shape of the native surface (exports + signatures)
/// that the TypeScript loader requires. Bump it ONLY on a backward-incompatible
/// change to that surface, and update `NATIVE_GREP_ABI_VERSION` in the loader +
/// the `__senpiGrepAbi<N>` sentinel export together. A CalVer release must NOT
/// change it — otherwise every release invalidates the prebuilt binaries.
pub const NATIVE_GREP_ABI_VERSION: &str = "1";

// Stable ABI sentinel. The `js_name` MUST be a string literal (napi requirement),
// so it is spelled `__senpiGrepAbi<N>` where N == NATIVE_GREP_ABI_VERSION. The
// loader derives the same name from its own ABI constant and also checks the
// return value, rejecting any prebuilt binary compiled against a different ABI.
pub fn senpi_grep_abi_sentinel() -> String {
    NATIVE_GREP_ABI_VERSION.to_string()
}

#[cfg(test)]
mod tests;
