//! Rust port of the `@oh-my-opencode/config-migration` package (SUL-1.0, internal use only).

mod category_deep_split;
mod deep_diff;
mod discovery;
mod discovery_paths;
mod discovery_roots;
mod harness_native_rename;
mod legacy_history;
mod migration_executor;
mod migration_plans;
mod path_posix;
mod path_win32;
mod reasoning_unification;
mod record_values;
mod schema_url;
mod subscription_provider_rename;
mod transform_config_jsonc;
mod transform_opencode;
mod transform_types;
mod types;

pub use category_deep_split::{
    CATEGORY_DEEP_SPLIT_MIGRATION_ID, transform_category_deep_split,
};
pub use discovery::{
    CONFIG_JSONC_MIGRATION_ID, OPENCODE_CONFIG_MIGRATION_ID, discover_legacy_config_groups,
};
pub use harness_native_rename::{
    HARNESS_NATIVE_RENAME_MIGRATION_ID, transform_harness_native_rename,
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
pub use subscription_provider_rename::{
    SUBSCRIPTION_PROVIDER_RENAME_MIGRATION_ID, SubscriptionProviderRewrite,
    has_legacy_subscription_provider_ids, transform_subscription_provider_rename,
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
