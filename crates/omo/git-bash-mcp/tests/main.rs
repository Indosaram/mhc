//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "binary_stdio.rs"]
mod binary_stdio;
#[path = "mcp.rs"]
mod mcp;
#[path = "resolver.rs"]
mod resolver;
#[path = "runner.rs"]
mod runner;
