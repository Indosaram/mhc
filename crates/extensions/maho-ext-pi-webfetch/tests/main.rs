//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "content.rs"]
mod content;
#[path = "discard_body.rs"]
mod discard_body;
#[path = "errors.rs"]
mod errors;
#[path = "fetcher.rs"]
mod fetcher;
#[path = "output_cap.rs"]
mod output_cap;
#[path = "registered.rs"]
mod registered;
#[path = "renderers.rs"]
mod renderers;
