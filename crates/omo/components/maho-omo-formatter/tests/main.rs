//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "cli_fallback.rs"]
mod cli_fallback;
#[path = "formatter.rs"]
mod formatter;
#[path = "policy.rs"]
mod policy;
#[path = "semantics_regression.rs"]
mod semantics_regression;
