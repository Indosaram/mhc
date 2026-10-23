//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "contributed_skill.rs"]
pub mod contributed_skill;
#[path = "resources_discover.rs"]
pub mod resources_discover;

// Helpers several test modules share, declared once so each file compiles once.
#[path = "support/mod.rs"]
pub mod support;
