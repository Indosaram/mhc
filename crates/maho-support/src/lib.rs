//! Small helpers shared by the MIT harness crates (clean-room rewrite, see `PROVENANCE.md`).
//!
//! Phase 1 surface: JSONC parsing, atomic and exclusive writes, path containment, a
//! process-wide log hook and a safe PATH lookup, plus a few string helpers.

pub mod atomic;
pub mod jsonc;
pub mod misc;
pub mod paths;

pub use atomic::{
    AtomicStep, unique_temp_path, write_atomically, write_atomically_with, write_exclusive,
};
pub use jsonc::{JsoncError, JsoncParse, parse_jsonc, parse_jsonc_lenient, read_jsonc_file};
pub use misc::{
    LogSink, escape_for_double_quotes, extract_semver, find_free_port, format_duration, log,
    merge_unique, merge_unique_ci, set_log_sink, which_safe, which_safe_in,
};
pub use paths::{
    canonicalize_lenient, contains_path, is_markdown_file, is_symlink, is_within,
    normalize_lexically, resolve_symlink,
};
