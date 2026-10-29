//! Test-only re-exports of messaging fakes so sibling `team` test modules can reuse them.

#[cfg(test)]
pub(crate) use super::messaging_fakes::state_dir_config;
