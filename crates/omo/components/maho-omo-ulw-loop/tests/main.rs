//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "command.rs"]
pub mod command;
#[path = "containment.rs"]
pub mod containment;
#[path = "default_runtime.rs"]
pub mod default_runtime;
#[path = "faux_session.rs"]
pub mod faux_session;
#[path = "footer.rs"]
pub mod footer;
#[path = "lifecycle.rs"]
pub mod lifecycle;
#[path = "runtime.rs"]
pub mod runtime;

// Helpers several test modules share, declared once so each file compiles once.
#[path = "support/mod.rs"]
pub mod support;
