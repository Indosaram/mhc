//! Replicated project spaces, insert-only adoption, and committed-item deletion gates.
#![deny(clippy::unwrap_used)]

pub mod adopt;
pub mod adopt_frontmatter;
pub mod binding;
pub mod confirm_removals;
pub mod hub_trace;
pub mod join;
pub mod manifest;
pub mod merge;
pub mod read_back;
#[path = "space.rs"]
pub mod resolution;

pub use adopt::*;
pub use binding::*;
pub use confirm_removals::*;
pub use hub_trace::*;
pub use join::{JoinDirection, JoinResult, join_direction, join_spaces};
pub use manifest::*;
pub use merge::{is_ordered_subsequence, merge_insertions, merge_insertions_with_budget};
pub use resolution as space;
pub use resolution::*;

#[cfg(test)]
mod manifest_tests;

#[derive(Debug)]
pub enum SpaceError {
    EmptyProfile,
    Entropy,
    Random,
    Remote(String),
    GitWrite(String),
    MissingComponents(String),
    Item(super::item::ItemError),
    Io(std::io::Error),
}
impl std::fmt::Display for SpaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyProfile => {
                f.write_str("namedBinding: profile value is empty after trimming")
            }
            Self::Entropy => {
                f.write_str("localBinding: entropy must be 128 bits as 32 lowercase hex characters")
            }
            Self::Random => f.write_str("failed to obtain binding entropy"),
            Self::Remote(message) => f.write_str(message),
            Self::GitWrite(key) => write!(f, "GitSpaceConfig: failed to write {key}"),
            Self::MissingComponents(key) => write!(
                f,
                "planAdoption: ItemState.registers has no components for register key {}",
                serde_json::Value::String(key.clone())
            ),
            Self::Item(error) => error.fmt(f),
            Self::Io(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for SpaceError {}
impl From<super::item::ItemError> for SpaceError {
    fn from(error: super::item::ItemError) -> Self {
        Self::Item(error)
    }
}
impl From<std::io::Error> for SpaceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod adopt_tests;

#[cfg(test)]
mod replay_tests;

#[cfg(test)]
mod preservation_tests;

#[cfg(test)]
mod fixture_tests;

#[cfg(test)]
mod readback_tests;

#[cfg(test)]
mod commit_gate_tests;

#[cfg(test)]
mod corpus_support;

#[cfg(test)]
mod adoption_corpus_tests;

#[cfg(test)]
mod readback_corpus_tests;

#[cfg(test)]
mod git_resolution_tests;
