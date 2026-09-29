//! Rust port of the `@oh-my-opencode/senpi-task` package (SUL-1.0, internal use only).

pub mod agents;
pub mod category;
pub mod completion;
pub mod dag;
mod delegate_adapter;
pub mod host;
pub mod lifecycle;
pub mod manager;
pub mod model_chain;
pub mod progress;
pub mod renderer_text;
pub mod run_stats;
pub mod runners;
pub mod senpi;
pub mod shared;
pub mod state;
pub mod status_line;
pub mod steering;
pub mod store;
pub mod task_summary;
#[cfg(test)]
mod test_support;
pub mod tools;

pub use delegate_adapter::DelegateFallbackEntry;
