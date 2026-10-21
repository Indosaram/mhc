//! Native port of `@earendil-works/chord` 0.99.1.
//!
//! JSON revisions use reference-counted containers. Mutation is copy-on-write;
//! previously published revisions and operation payloads remain immutable.
pub mod api;
#[path = "context/index.rs"]
pub mod context;
#[path = "delta/index.rs"]
pub mod delta;
pub mod js_string;
pub mod json;
pub mod services;
pub mod types;
pub mod facets {
    pub mod host;
    pub mod loader;
}
pub mod index;

pub use index::*;
