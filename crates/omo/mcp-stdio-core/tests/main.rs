//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "server.rs"]
mod server;
#[path = "transport.rs"]
mod transport;
