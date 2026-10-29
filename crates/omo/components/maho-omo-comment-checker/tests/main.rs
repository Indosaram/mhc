//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "component.rs"]
pub mod component;
#[path = "downloader.rs"]
pub mod downloader;
#[path = "faux_session.rs"]
pub mod faux_session;
#[path = "hook_input.rs"]
pub mod hook_input;
#[path = "resolver.rs"]
pub mod resolver;
#[path = "runner.rs"]
pub mod runner;
#[path = "stale_cache.rs"]
pub mod stale_cache;
#[path = "start_failure.rs"]
pub mod start_failure;
#[path = "stdin_pipe.rs"]
pub mod stdin_pipe;
#[path = "utils.rs"]
pub mod utils;

// Helpers several test modules share, declared once so each file compiles once.
#[path = "support/mod.rs"]
pub mod support;
