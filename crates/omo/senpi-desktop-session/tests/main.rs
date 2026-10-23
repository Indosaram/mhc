//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "capture_budget.rs"]
mod capture_budget;
#[path = "session.rs"]
mod session;
#[path = "session_options.rs"]
mod session_options;
