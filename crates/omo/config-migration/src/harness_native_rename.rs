use omo_config_core::canonicalize_legacy_harness_blocks;
use serde_json::Value;

use crate::transform_types::ConfigMigrationTransformResult;

pub const HARNESS_NATIVE_RENAME_MIGRATION_ID: &str = "2026-09-harness-native-rename";

pub fn transform_harness_native_rename(document: &Value) -> ConfigMigrationTransformResult {
    let canonicalized = canonicalize_legacy_harness_blocks(document);
    ConfigMigrationTransformResult {
        diagnostics: canonicalized
            .renames
            .iter()
            .map(|rename| {
                if rename.dropped {
                    format!(
                        "{} removed: {} is already configured",
                        rename.path, rename.canonical
                    )
                } else {
                    format!("{} renamed to {}", rename.path, rename.canonical)
                }
            })
            .collect(),
        document: canonicalized.document,
    }
}
