//! Dependency-free primitives shared by every memory-core module.

pub mod host;
pub mod paths;
pub mod random;
pub mod sha256;
pub mod time;

#[cfg(test)]
pub(crate) mod test_repo;
