//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "call.rs"]
mod call;
#[path = "method_allowlist.rs"]
mod method_allowlist;
#[path = "schema_drift.rs"]
mod schema_drift;
