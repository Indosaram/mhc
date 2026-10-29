//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "native_engine.rs"]
mod native_engine;
#[path = "service.rs"]
mod service;
#[path = "trust_boundary.rs"]
mod trust_boundary;
