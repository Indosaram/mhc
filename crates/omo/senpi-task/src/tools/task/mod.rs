//! `tools/task/`: the `task` spawn tool.

pub mod argument_normalization;
#[cfg(test)]
mod argument_normalization_tests;
pub mod call_renderer;
pub mod categories;
#[cfg(test)]
mod categories_tests;
pub mod description;
#[cfg(test)]
mod description_plan_gated_tests;
#[cfg(test)]
mod description_tests;
pub mod execute;
#[cfg(test)]
mod execute_abort_tests;
pub mod execute_batch;
#[cfg(test)]
mod execute_batch_background_tests;
#[cfg(test)]
mod execute_batch_tests;
#[cfg(test)]
mod execute_budget_wait_tests;
#[cfg(test)]
mod execute_collision_tests;
#[cfg(test)]
mod execute_continuation_scope_tests;
#[cfg(test)]
mod execute_continuation_tests;
#[cfg(test)]
mod execute_invocation_guard_tests;
#[cfg(test)]
mod execute_plan_error_tests;
#[cfg(test)]
mod execute_plan_review_contract_tests;
#[cfg(test)]
mod execute_progress_tests;
pub mod execute_single;
#[cfg(test)]
mod execute_skills_tests;
#[cfg(test)]
mod execute_spawn_tests;
#[cfg(test)]
mod execute_spawn_validation_tests;
pub mod execute_spec;
pub mod foreground_wait;
#[cfg(test)]
mod gated_categories_tests;
#[cfg(test)]
mod integration_tests;
pub mod invocation_gate;
#[cfg(test)]
mod model_visibility_tests;
#[cfg(test)]
mod momus_policy_integration_tests;
pub mod params;
#[cfg(test)]
mod params_tests;
pub mod plan_review_contract;
pub mod renderers;
#[cfg(test)]
mod renderers_tests;
pub mod result_details;
#[cfg(test)]
mod result_details_tests;
pub mod skill_result;
pub mod skills;
#[cfg(test)]
mod skills_tests;
pub mod spawn_policy;
pub mod start_presentation;
#[cfg(test)]
mod start_presentation_tests;
#[cfg(test)]
mod task_tool_fakes;
pub mod tool;
#[cfg(test)]
mod tool_tests;
pub mod types;
pub mod validation;
