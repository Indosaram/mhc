use omo_config_core::canonicalize_legacy_category_names;
use serde_json::Value;

use crate::transform_types::ConfigMigrationTransformResult;

pub const CATEGORY_DEEP_SPLIT_MIGRATION_ID: &str = "2026-09-category-deep-split";

pub fn transform_category_deep_split(document: &Value) -> ConfigMigrationTransformResult {
    let canonicalized = canonicalize_legacy_category_names(document);
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
