//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "apply_patch_edits.rs"]
mod apply_patch_edits;
#[path = "cached_release.rs"]
mod cached_release;
#[path = "release.rs"]
mod release;
#[path = "runner.rs"]
mod runner;
#[path = "runner_exit_protocol.rs"]
mod runner_exit_protocol;
#[path = "runner_process.rs"]
mod runner_process;
#[path = "runner_stdin.rs"]
mod runner_stdin;
