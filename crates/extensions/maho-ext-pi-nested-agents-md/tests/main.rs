//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "filesystem.rs"]
mod filesystem;
#[path = "find_agents.rs"]
mod find_agents;
#[path = "lifecycle.rs"]
mod lifecycle;
