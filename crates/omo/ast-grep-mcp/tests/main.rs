//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "binary_stdio.rs"]
pub mod binary_stdio;
#[path = "mcp.rs"]
pub mod mcp;
#[path = "normalize.rs"]
pub mod normalize;
#[path = "pattern_hints.rs"]
pub mod pattern_hints;
#[path = "rewrite.rs"]
pub mod rewrite;
#[path = "scan.rs"]
pub mod scan;
#[path = "search.rs"]
pub mod search;
#[path = "sg_runner.rs"]
pub mod sg_runner;

// Helpers several test modules share, declared once so each file compiles once.
#[path = "common/mod.rs"]
pub mod common;
