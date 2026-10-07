use serde_json::{Value, json};

use crate::internal::validate::{
    DefaultFn, Node, any, array, defaulted, optional, record, strict_object, string,
};
use crate::schema::agent::omo_agents_config_schema;
use crate::schema::category::omo_categories_config_schema;
use crate::schema::computer::{
    omo_computer_settings_layer_schema, omo_computer_settings_schema,
};
use crate::schema::format_on_mutation::{
    omo_format_on_mutation_layer_schema, omo_format_on_mutation_schema,
};
use crate::schema::gateway::omo_gateway_section_schema;
use crate::schema::git_master::{
    omo_git_master_settings_layer_schema, omo_git_master_settings_schema,
};
use crate::schema::memory::{omo_memory_settings_layer_schema, omo_memory_settings_schema};
use crate::schema::model_catalog::{omo_model_catalog_layer_schema, omo_model_catalog_schema};
use crate::schema::model_profile::{
    omo_model_profiles_layer_schema, omo_model_profiles_schema,
};
use crate::schema::task::{omo_task_settings_layer_schema, omo_task_settings_schema};
use crate::schema::team::{omo_teams_config_layer_schema, omo_teams_config_schema};
use crate::schema::telemetry::{
    omo_telemetry_settings_layer_schema, omo_telemetry_settings_schema,
};

pub fn default_profiles() -> Value {
    json!({})
}

pub fn omo_open_code_harness_config_schema() -> Node {
    record(any())
}

/// Canonical skill denylist. Names listed here are absent from the run on every harness that
/// loads the skill; layers (user, project, `[harness]`, profile) are unioned, never replaced.
pub fn omo_disabled_skills_schema() -> Node {
    array(string())
}

pub fn omo_typed_harness_config_schema() -> Node {
    strict_object(vec![
        optional("formatOnMutation", omo_format_on_mutation_layer_schema()),
        optional("gateway", omo_gateway_section_schema()),
        optional("categories", omo_categories_config_schema()),
        optional("agents", omo_agents_config_schema()),
        optional("git_master", omo_git_master_settings_layer_schema()),
        optional("task", omo_task_settings_layer_schema()),
        optional("teams", omo_teams_config_layer_schema()),
        optional("models", omo_model_catalog_layer_schema()),
        optional("model_profiles", omo_model_profiles_layer_schema()),
        optional("model_profile", string()),
        optional("memory", omo_memory_settings_layer_schema()),
        optional("telemetry", omo_telemetry_settings_layer_schema()),
        optional("computer", omo_computer_settings_layer_schema()),
        optional("disabled_skills", omo_disabled_skills_schema()),
    ])
}

pub fn omo_config_profile_schema() -> Node {
    strict_object(vec![
        optional("formatOnMutation", omo_format_on_mutation_layer_schema()),
        optional("categories", omo_categories_config_schema()),
        optional("agents", omo_agents_config_schema()),
        optional("git_master", omo_git_master_settings_layer_schema()),
        optional("task", omo_task_settings_layer_schema()),
        optional("teams", omo_teams_config_layer_schema()),
        optional("models", omo_model_catalog_layer_schema()),
        optional("model_profiles", omo_model_profiles_layer_schema()),
        optional("model_profile", string()),
        optional("memory", omo_memory_settings_layer_schema()),
        optional("telemetry", omo_telemetry_settings_layer_schema()),
        optional("computer", omo_computer_settings_layer_schema()),
        optional("disabled_skills", omo_disabled_skills_schema()),
        optional("[opencode]", omo_open_code_harness_config_schema()),
        optional("[native]", omo_typed_harness_config_schema()),
        optional("[senpi]", omo_typed_harness_config_schema()),
        optional("[codex]", omo_typed_harness_config_schema()),
    ])
}

pub fn omo_config_schema() -> Node {
    strict_object(vec![
        optional("formatOnMutation", omo_format_on_mutation_schema()),
        optional("gateway", omo_gateway_section_schema()),
        optional("$schema", string()),
        optional("categories", omo_categories_config_schema()),
        optional("agents", omo_agents_config_schema()),
        optional("git_master", omo_git_master_settings_schema()),
        optional("task", omo_task_settings_schema()),
        optional("teams", omo_teams_config_schema()),
        optional("models", omo_model_catalog_schema()),
        optional("model_profiles", omo_model_profiles_schema()),
        optional("model_profile", string()),
        optional("memory", omo_memory_settings_schema()),
        optional("telemetry", omo_telemetry_settings_schema()),
        optional("computer", omo_computer_settings_schema()),
        optional("disabled_skills", omo_disabled_skills_schema()),
        optional("[opencode]", omo_open_code_harness_config_schema()),
        optional("[native]", omo_typed_harness_config_schema()),
        optional("[senpi]", omo_typed_harness_config_schema()),
        optional("[codex]", omo_typed_harness_config_schema()),
        defaulted(
            "profiles",
            record(omo_config_profile_schema()),
            default_profiles as DefaultFn,
        ),
        optional("_migrations", array(string())),
        optional("legacy_migrations", record(any())),
    ])
}

pub fn omo_config_layer_schema() -> Node {
    strict_object(vec![
        optional("formatOnMutation", omo_format_on_mutation_layer_schema()),
        optional("gateway", omo_gateway_section_schema()),
        optional("$schema", string()),
        optional("categories", omo_categories_config_schema()),
        optional("agents", omo_agents_config_schema()),
        optional("git_master", omo_git_master_settings_layer_schema()),
        optional("task", omo_task_settings_layer_schema()),
        optional("teams", omo_teams_config_layer_schema()),
        optional("models", omo_model_catalog_layer_schema()),
        optional("model_profiles", omo_model_profiles_layer_schema()),
        optional("model_profile", string()),
        optional("memory", omo_memory_settings_layer_schema()),
        optional("telemetry", omo_telemetry_settings_layer_schema()),
        optional("computer", omo_computer_settings_layer_schema()),
        optional("disabled_skills", omo_disabled_skills_schema()),
        optional("[opencode]", omo_open_code_harness_config_schema()),
        optional("[native]", omo_typed_harness_config_schema()),
        optional("[senpi]", omo_typed_harness_config_schema()),
        optional("[codex]", omo_typed_harness_config_schema()),
        optional("profiles", record(omo_config_profile_schema())),
        optional("_migrations", array(string())),
        optional("legacy_migrations", record(any())),
    ])
}
