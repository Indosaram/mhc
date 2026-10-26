//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "ax.rs"]
pub mod ax;
#[path = "capture.rs"]
pub mod capture;
#[path = "input.rs"]
pub mod input;
#[path = "scenario.rs"]
pub mod scenario;

// Helpers several test modules share, declared once so each file compiles once.
#[path = "common/mod.rs"]
pub mod common;
