//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "commands.rs"]
mod commands;
#[path = "policy.rs"]
mod policy;
