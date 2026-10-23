//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "client.rs"]
mod client;
#[path = "reporter.rs"]
mod reporter;
#[path = "state.rs"]
mod state;
