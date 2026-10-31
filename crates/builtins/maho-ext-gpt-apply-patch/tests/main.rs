//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "diff_source_cases.rs"]
mod diff_source_cases;
#[path = "line_endings.rs"]
mod line_endings;
#[path = "native_patch.rs"]
mod native_patch;
#[path = "render_patch.rs"]
mod render_patch;
#[path = "source_cases.rs"]
mod source_cases;
