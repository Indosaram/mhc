//! In-process child sessions driven through the host session seam.

pub mod child_handle;
pub mod child_options;
pub mod curated_readonly_bash;
pub mod runner;
pub mod runner_error;
pub mod runtime_fallback_settings;
pub mod session_manager;
pub mod shared_tool_filter;
pub mod subagent_prompt;

pub use runner::{ChildSpec, InProcessRunner, InProcessRunnerOptions};

#[cfg(test)]
mod in_process_tests;
