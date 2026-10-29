//! `tools/team/`: the lead-only team tools.

#[cfg(test)]
mod allowlist_tests;
pub mod classify_error;
pub mod index;
pub mod lifecycle;
#[cfg(test)]
mod lifecycle_collision_tests;
#[cfg(test)]
mod lifecycle_precedence_tests;
#[cfg(test)]
mod lifecycle_tests;
pub mod messaging;
#[cfg(test)]
mod messaging_tests;
pub mod query;
#[cfg(test)]
mod query_tests;
pub mod shutdown;
#[cfg(test)]
mod shutdown_tests;
pub mod tasks;
#[cfg(test)]
mod tasks_tests;
#[cfg(test)]
pub(crate) mod team_tool_fakes;
pub mod types;
