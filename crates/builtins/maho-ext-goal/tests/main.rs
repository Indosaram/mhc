//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "faux_native.rs"]
mod faux_native;
#[path = "public_store.rs"]
mod public_store;
