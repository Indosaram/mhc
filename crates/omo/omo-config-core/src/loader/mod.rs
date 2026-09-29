#[allow(clippy::module_inception)] // mirrors the TS loader/loader.ts layout
pub mod loader;
pub mod merge;
pub mod paths;
pub mod resolution;
pub mod types;

pub use loader::*;
pub use merge::*;
pub use paths::*;
pub use resolution::*;
pub use types::*;
