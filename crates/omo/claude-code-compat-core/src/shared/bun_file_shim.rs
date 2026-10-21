//! Native filesystem equivalent of the upstream runtime file adapter.
pub use utils::runtime::{RuntimeFile, bun_file, bun_write};
pub type BunFileLike = RuntimeFile;
