//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "cli.rs"]
pub mod cli;
#[path = "daemon_client.rs"]
pub mod daemon_client;
#[path = "daemon_server.rs"]
pub mod daemon_server;
#[path = "legacy_path_vectors.rs"]
pub mod legacy_path_vectors;
#[path = "proxy.rs"]
pub mod proxy;

// Helpers several test modules share, declared once so each file compiles once.
#[path = "common/mod.rs"]
pub mod common;
