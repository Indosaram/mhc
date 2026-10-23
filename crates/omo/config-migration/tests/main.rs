//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "discovery.rs"]
mod discovery;
#[path = "plans.rs"]
mod plans;
#[path = "transforms.rs"]
mod transforms;
