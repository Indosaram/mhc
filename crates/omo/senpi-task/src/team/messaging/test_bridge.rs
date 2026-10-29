//! Test-only bridge that exposes messaging fixtures to sibling `team` tests.
//!
//! `messaging_fakes` is a private `#[cfg(test)]` module of `team::messaging`,
//! so tests outside this directory reach its helpers through this re-export.

#[cfg(test)]
pub(crate) use super::messaging_fakes::state_dir_config;
