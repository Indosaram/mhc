//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "eligibility.rs"]
pub mod eligibility;
#[path = "faux_session.rs"]
pub mod faux_session;
#[path = "lifecycle.rs"]
pub mod lifecycle;
#[path = "terminal_outcome.rs"]
pub mod terminal_outcome;

// Helpers several test modules share, declared once so each file compiles once.
#[path = "support/mod.rs"]
pub mod support;
