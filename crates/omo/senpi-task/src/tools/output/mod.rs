//! `tools/output/`: the task_output tool.

#[cfg(test)]
mod factory_tests;
#[cfg(test)]
mod integration_tests;
#[expect(clippy::module_inception, reason = "mirrors the TS module path")]
pub mod output;
#[cfg(test)]
mod output_block_tests;
#[cfg(test)]
mod output_tests;
#[cfg(test)]
mod records_fakes;
pub mod render;
#[cfg(test)]
mod render_tests;
pub mod renderers;
#[cfg(test)]
mod renderers_run_stats_tests;
#[cfg(test)]
mod renderers_tests;
pub mod snapshot;
pub mod types;
pub mod transcript;
