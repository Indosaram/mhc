//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "harness_neutrality.rs"]
mod harness_neutrality;
#[path = "receipt_processes.rs"]
mod receipt_processes;
