//! `team/`: the senpi team runtime (slice C).

pub mod errors;
#[cfg(test)]
mod fallback_notification_tests;
#[cfg(test)]
mod import_discipline_tests;
pub mod liveness_ownership;
pub mod member_extensions;
#[cfg(test)]
mod member_extensions_tests;
pub mod member_map;
#[cfg(test)]
mod member_map_tests;
pub mod member_projection;
#[cfg(test)]
mod member_projection_tests;
pub mod member_respawn;
#[cfg(test)]
mod member_respawn_tests;
#[cfg(test)]
mod member_revival_tests;
pub mod member_validator;
#[cfg(test)]
mod member_validator_tests;
pub mod normalize;
#[cfg(test)]
mod normalize_tests;
pub mod registry;
#[cfg(test)]
mod registry_tests;
pub mod runtime;
pub mod runtime_config;
#[cfg(test)]
mod runtime_config_tests;
#[cfg(test)]
mod runtime_create_failure_tests;
#[cfg(test)]
mod runtime_delete_tests;
#[cfg(test)]
mod runtime_fakes;
#[cfg(test)]
mod runtime_tests;
pub mod runtime_types;
pub mod shutdown;
pub mod shutdown_helpers;
#[cfg(test)]
mod shutdown_helpers_tests;
#[cfg(test)]
mod shutdown_tests;
pub mod spawn_members;
#[cfg(test)]
mod spawn_members_task_summary_tests;
pub mod storage;
#[cfg(test)]
mod storage_tests;
pub mod tasks;
#[cfg(test)]
mod tasks_tests;
pub mod member_extension;
pub mod messaging;
