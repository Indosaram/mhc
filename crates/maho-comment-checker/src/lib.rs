//! Comment-checker hook support: the JSON a `comment-checker` binary reads on stdin, helpers to
//! read edit payloads out of tool inputs, and lookup of the native checker binary.
//!
//! Ported from MIT `pi-comment-checker` and `@code-yeongyu/comment-checker` (see PROVENANCE.md).

mod binary;
mod hook;

pub use binary::{
    BINARY_ENV, BinaryLookup, PACKAGE_NAME, binary_name, platform_key, resolve_binary,
    resolve_package_binary,
};
pub use hook::{
    ApplyPatchFile, EXIT_PASS, EXIT_WARNING, EditPair, HookInput, HookToolInput,
    get_apply_patch_metadata_files, get_string,
};
