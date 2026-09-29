//! Legacy config migrations (agent/hook names, model versions, sidecar history).

mod agent_category;
mod agent_names;
mod config_migration;
mod hook_names;
mod migrations_sidecar;
mod model_versions;

pub use agent_category::{
    MODEL_TO_CATEGORY_MAP, configure_migration_category_defaults, migrate_agent_config_to_category,
    should_delete_agent_config,
};
pub use agent_names::{AGENT_NAME_MAP, BUILTIN_AGENT_NAMES, agent_name_for, migrate_agent_names};
pub use config_migration::migrate_config_file;
pub use hook_names::{
    HOOK_NAME_MAP, HookMigration, HookRename, hook_name_mapping, migrate_hook_names,
};
pub use migrations_sidecar::{
    MigrationsSidecar, get_sidecar_path, read_applied_migrations, write_applied_migrations,
};
pub use model_versions::{MODEL_VERSION_MAP, ModelVersionMigration, migrate_model_versions};

use serde_json::{Map, Value};

/// Result of a key-renaming or value-rewriting migration over a JSON object.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectMigration {
    pub migrated: Map<String, Value>,
    pub changed: bool,
}
