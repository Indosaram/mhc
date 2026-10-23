//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "baseline.rs"]
pub mod baseline;
#[path = "config_reload_harness.rs"]
pub mod config_reload_harness;
#[path = "index.rs"]
pub mod index;
#[path = "loader.rs"]
pub mod loader;
#[path = "native_registration.rs"]
pub mod native_registration;
#[path = "paths.rs"]
pub mod paths;
#[path = "validation.rs"]
pub mod validation;

// Helpers several test modules share, declared once so each file compiles once.
#[path = "support/mod.rs"]
pub mod support;
