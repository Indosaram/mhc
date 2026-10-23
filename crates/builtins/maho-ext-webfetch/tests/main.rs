//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "dom_source_cases.rs"]
mod dom_source_cases;
#[path = "native_render.rs"]
mod native_render;
