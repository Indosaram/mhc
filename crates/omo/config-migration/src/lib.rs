//! Rust port of the `@oh-my-opencode/config-migration` package (SUL-1.0, internal use only).

mod deep_diff;
mod discovery;
mod discovery_paths;
mod discovery_roots;
mod legacy_history;
mod migration_executor;
mod migration_plans;
mod path_posix;
mod path_win32;
mod reasoning_unification;
mod record_values;
mod schema_url;
mod transform_config_jsonc;
mod transform_opencode;
mod transform_types;
mod types;

pub use discovery::{
    CONFIG_JSONC_MIGRATION_ID, OPENCODE_CONFIG_MIGRATION_ID, discover_legacy_config_groups,
};
pub use migration_executor::{
    ExecuteLegacyConfigMigrationPlanOptions, execute_legacy_config_migration_plan,
};
pub use migration_plans::{
    CreateLegacyConfigMigrationPlansOptions, LegacyConfigMigrationPlan,
    LegacyConfigMigrationTransform, create_legacy_config_migration_plans,
};
pub use reasoning_unification::{
    REASONING_UNIFICATION_MIGRATION_ID, ReasoningUnificationError, transform_reasoning_unification,
};
pub use transform_config_jsonc::transform_config_jsonc_sources;
pub use transform_opencode::transform_open_code_sources;
pub use transform_types::{
    ConfigMigrationTransformResult, LoadedLegacyConfigSource, OpenCodeTransformScope,
    TransformConfigJsoncSourcesInput, TransformOpenCodeSourcesInput,
};
pub use types::{
    ConfigMigrationDiscoveryFileSystem, ConfigMigrationDiscoveryOptions,
    DiscoveredLegacyConfigSource, DiscoveryFsError, DiscoveryFsErrorCode,
    LegacyConfigMigrationGroup, LegacyConfigSourceKind, PathOperations, Platform,
    StdDiscoveryFileSystem,
};
