use serde_json::{Value, json};

use crate::internal::validate::{
    DefaultFn, Node, any, array, defaulted, optional, record, strict_object, string,
};
use crate::schema::agent::omo_agents_config_schema;
use crate::schema::category::omo_categories_config_schema;
use crate::schema::codegraph::{
    omo_codegraph_settings_layer_schema, omo_codegraph_settings_schema,
};
use crate::schema::memory::{omo_memory_settings_layer_schema, omo_memory_settings_schema};
use crate::schema::model_catalog::{omo_model_catalog_layer_schema, omo_model_catalog_schema};
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

pub fn omo_typed_harness_config_schema() -> Node {
    strict_object(vec![
        optional("categories", omo_categories_config_schema()),
        optional("agents", omo_agents_config_schema()),
        optional("codegraph", omo_codegraph_settings_layer_schema()),
        optional("task", omo_task_settings_layer_schema()),
        optional("teams", omo_teams_config_layer_schema()),
        optional("models", omo_model_catalog_layer_schema()),
        optional("memory", omo_memory_settings_layer_schema()),
        optional("telemetry", omo_telemetry_settings_layer_schema()),
    ])
}

pub fn omo_config_profile_schema() -> Node {
    strict_object(vec![
        optional("categories", omo_categories_config_schema()),
        optional("agents", omo_agents_config_schema()),
        optional("codegraph", omo_codegraph_settings_layer_schema()),
        optional("task", omo_task_settings_layer_schema()),
        optional("teams", omo_teams_config_layer_schema()),
        optional("models", omo_model_catalog_layer_schema()),
        optional("memory", omo_memory_settings_layer_schema()),
        optional("telemetry", omo_telemetry_settings_layer_schema()),
        optional("[opencode]", omo_open_code_harness_config_schema()),
        optional("[senpi]", omo_typed_harness_config_schema()),
        optional("[codex]", omo_typed_harness_config_schema()),
    ])
}

pub fn omo_config_schema() -> Node {
    strict_object(vec![
        optional("$schema", string()),
        optional("categories", omo_categories_config_schema()),
        optional("agents", omo_agents_config_schema()),
        optional("codegraph", omo_codegraph_settings_schema()),
        optional("task", omo_task_settings_schema()),
        optional("teams", omo_teams_config_schema()),
        optional("models", omo_model_catalog_schema()),
        optional("memory", omo_memory_settings_schema()),
        optional("telemetry", omo_telemetry_settings_schema()),
        optional("[opencode]", omo_open_code_harness_config_schema()),
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
        optional("$schema", string()),
        optional("categories", omo_categories_config_schema()),
        optional("agents", omo_agents_config_schema()),
        optional("codegraph", omo_codegraph_settings_layer_schema()),
        optional("task", omo_task_settings_layer_schema()),
        optional("teams", omo_teams_config_layer_schema()),
        optional("models", omo_model_catalog_layer_schema()),
        optional("memory", omo_memory_settings_layer_schema()),
        optional("telemetry", omo_telemetry_settings_layer_schema()),
        optional("[opencode]", omo_open_code_harness_config_schema()),
        optional("[senpi]", omo_typed_harness_config_schema()),
        optional("[codex]", omo_typed_harness_config_schema()),
        optional("profiles", record(omo_config_profile_schema())),
        optional("_migrations", array(string())),
        optional("legacy_migrations", record(any())),
    ])
}
