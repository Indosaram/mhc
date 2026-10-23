//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "directive.rs"]
mod directive;
#[path = "index.rs"]
mod index;
