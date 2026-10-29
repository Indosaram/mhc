//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "composition_adapter.rs"]
mod composition_adapter;
#[path = "native_memory.rs"]
mod native_memory;
