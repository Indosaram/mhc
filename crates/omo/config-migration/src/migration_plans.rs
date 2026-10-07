use std::collections::HashMap;
use std::fmt::Write as _;
use std::rc::Rc;

use omo_config_core::{
    LoadedMigrationSource, MigrationError, MigrationMode, MigrationSourceDescriptor,
    has_legacy_category_names, has_legacy_harness_blocks,
};
use serde_json::{Map, Value};

use crate::category_deep_split::{
    CATEGORY_DEEP_SPLIT_MIGRATION_ID, transform_category_deep_split,
};
use crate::discovery::{
    CONFIG_JSONC_MIGRATION_ID, OPENCODE_CONFIG_MIGRATION_ID, discover_legacy_config_groups,
};
use crate::discovery_paths::{
    canonical_path, discovery_file_system, host_path_operations, path_key, project_directories,
};
use crate::harness_native_rename::{
    HARNESS_NATIVE_RENAME_MIGRATION_ID, transform_harness_native_rename,
};
use crate::reasoning_unification::{
    REASONING_UNIFICATION_MIGRATION_ID, transform_reasoning_unification,
};
use crate::record_values::merge_records;
use crate::subscription_provider_rename::{
    SUBSCRIPTION_PROVIDER_RENAME_MIGRATION_ID, has_legacy_subscription_provider_ids,
    transform_subscription_provider_rename,
};
use crate::transform_config_jsonc::transform_config_jsonc_sources;
use crate::transform_opencode::transform_open_code_sources;
use crate::transform_types::{
    ConfigMigrationTransformResult, LoadedLegacyConfigSource, OpenCodeTransformScope,
    TransformConfigJsoncSourcesInput, TransformOpenCodeSourcesInput,
};
use crate::types::{
    ConfigMigrationDiscoveryOptions, DiscoveredLegacyConfigSource, DiscoveryFsError,
};

pub type LegacyConfigMigrationTransform =
    Rc<dyn Fn(&[LoadedMigrationSource]) -> Result<ConfigMigrationTransformResult, MigrationError>>;
pub type LegacyConfigMigrationPredicate = Rc<dyn Fn(&Value) -> bool>;

#[derive(Clone)]
pub struct LegacyConfigMigrationPlan {
    pub id: String,
    pub inspect: LegacyConfigMigrationTransform,
    pub mode: MigrationMode,
    pub should_run: Option<LegacyConfigMigrationPredicate>,
    pub sources: Vec<MigrationSourceDescriptor>,
    pub target_path: String,
    pub transform: LegacyConfigMigrationTransform,
}

#[derive(Clone, Copy)]
pub struct CreateLegacyConfigMigrationPlansOptions<'a> {
    pub backup_timestamp: Option<&'a str>,
    pub discovery: ConfigMigrationDiscoveryOptions<'a>,
}

#[derive(Clone)]
struct OpenCodePlanInput {
    scopes: Vec<OpenCodeTransformScope>,
    sources: Vec<DiscoveredLegacyConfigSource>,
    target_path: String,
}

fn backup_timestamp(value: Option<&str>) -> String {
    value.map_or_else(
        || {
            chrono::Utc::now()
                .format("%Y-%m-%dT%H-%M-%S-%3fZ")
                .to_string()
        },
        str::to_string,
    )
}

/// JavaScript `encodeURIComponent`: keeps `A-Z a-z 0-9 - _ . ! ~ * ' ( )`, percent-encodes UTF-8 bytes otherwise.
fn encode_uri_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn safe_relative_path(
    path: &str,
    home_dir: &str,
    options: &ConfigMigrationDiscoveryOptions<'_>,
) -> String {
    let path_operations = host_path_operations(options);
    let relative = path_operations.relative(home_dir, path);
    if !relative.is_empty()
        && !relative.starts_with("..")
        && !path_operations.is_absolute(&relative)
    {
        return relative;
    }
    format!("external-{}", encode_uri_component(path))
}

fn descriptors(
    sources: &[DiscoveredLegacyConfigSource],
    options: &ConfigMigrationDiscoveryOptions<'_>,
    timestamp: &str,
) -> Result<Vec<MigrationSourceDescriptor>, DiscoveryFsError> {
    let home_dir = canonical_path(options.home_dir, options)?;
    let path_operations = host_path_operations(options);
    let user_backup_root = path_operations.join(&[
        &home_dir,
        ".maho",
        &format!("migration-backup-{timestamp}-opencode-config"),
    ]);
    Ok(sources
        .iter()
        .map(|source| {
            let backup_path = match &source.project_root {
                None => path_operations.join(&[
                    &user_backup_root,
                    &safe_relative_path(&source.path, &home_dir, options),
                ]),
                Some(project_root) => path_operations.join(&[
                    project_root,
                    ".omo",
                    &format!("migration-backup-{timestamp}"),
                    &path_operations.basename(&source.path),
                ]),
            };
            MigrationSourceDescriptor::with_backup(source.path.clone(), backup_path)
        })
        .collect())
}

fn legacy_sources(loaded: &[LoadedMigrationSource]) -> Vec<LoadedLegacyConfigSource> {
    loaded
        .iter()
        .map(|source| LoadedLegacyConfigSource {
            path: source.path.clone(),
            value: source.value.clone(),
        })
        .collect()
}

fn open_code_inspection(
    input: &OpenCodePlanInput,
    loaded: &[LoadedMigrationSource],
) -> ConfigMigrationTransformResult {
    let sources = legacy_sources(loaded);
    let mut diagnostics = Vec::new();
    let mut document = Map::new();
    for scope in &input.scopes {
        let next = transform_open_code_sources(&TransformOpenCodeSourcesInput {
            discovered: &input.sources,
            scope,
            sources: &sources,
        });
        diagnostics.extend(next.diagnostics);
        document = merge_records(&document, &next.document);
    }
    ConfigMigrationTransformResult {
        diagnostics,
        document,
    }
}

fn open_code_plan(
    input: OpenCodePlanInput,
    options: &ConfigMigrationDiscoveryOptions<'_>,
    timestamp: &str,
) -> Result<LegacyConfigMigrationPlan, DiscoveryFsError> {
    let sources = descriptors(&input.sources, options, timestamp)?;
    let target_path = input.target_path.clone();
    let transform: LegacyConfigMigrationTransform =
        Rc::new(move |loaded| Ok(open_code_inspection(&input, loaded)));
    Ok(LegacyConfigMigrationPlan {
        id: OPENCODE_CONFIG_MIGRATION_ID.to_string(),
        inspect: Rc::clone(&transform),
        mode: MigrationMode::Merge,
        should_run: None,
        sources,
        target_path,
        transform,
    })
}

fn merge_open_code_inputs(
    inputs: Vec<OpenCodePlanInput>,
    options: &ConfigMigrationDiscoveryOptions<'_>,
) -> Result<Vec<OpenCodePlanInput>, DiscoveryFsError> {
    let mut merged: Vec<OpenCodePlanInput> = Vec::new();
    let mut index_by_key: HashMap<String, usize> = HashMap::new();
    for input in inputs {
        let key = path_key(&input.target_path, options)?;
        match index_by_key.get(&key) {
            Some(&index) => {
                let existing = &mut merged[index];
                existing.scopes.extend(input.scopes);
                existing.sources.extend(input.sources);
            }
            None => {
                index_by_key.insert(key, merged.len());
                merged.push(input);
            }
        }
    }
    Ok(merged)
}

fn config_jsonc_plan(
    discovered: Vec<DiscoveredLegacyConfigSource>,
    options: &ConfigMigrationDiscoveryOptions<'_>,
    timestamp: &str,
    target_path: String,
) -> Result<LegacyConfigMigrationPlan, DiscoveryFsError> {
    let sources = descriptors(&discovered, options, timestamp)?;
    let transform: LegacyConfigMigrationTransform = Rc::new(move |loaded| {
        let sources = legacy_sources(loaded);
        Ok(transform_config_jsonc_sources(
            &TransformConfigJsoncSourcesInput {
                discovered: &discovered,
                sources: &sources,
            },
        ))
    });
    Ok(LegacyConfigMigrationPlan {
        id: CONFIG_JSONC_MIGRATION_ID.to_string(),
        inspect: Rc::clone(&transform),
        mode: MigrationMode::Merge,
        should_run: None,
        sources,
        target_path,
        transform,
    })
}

fn reasoning_plan(target_path: String) -> LegacyConfigMigrationPlan {
    let transform: LegacyConfigMigrationTransform = Rc::new(|loaded| {
        transform_reasoning_unification(loaded.first().map(|source| &source.value))
            .map_err(|error| MigrationError::transaction(error.to_string()))
    });
    LegacyConfigMigrationPlan {
        id: REASONING_UNIFICATION_MIGRATION_ID.to_string(),
        inspect: Rc::clone(&transform),
        mode: MigrationMode::ReplaceTarget,
        should_run: None,
        sources: Vec::new(),
        target_path,
        transform,
    }
}

// Gated on content, unlike the reasoning plan: a config that never named a retired category is left
// untouched - no backup, no journal, no `_migrations` marker - instead of being rewritten to itself.
fn category_deep_split_plan(target_path: String) -> LegacyConfigMigrationPlan {
    let transform: LegacyConfigMigrationTransform = Rc::new(|loaded| {
        let value = loaded
            .first()
            .map(|source| source.value.clone())
            .unwrap_or(Value::Null);
        Ok(transform_category_deep_split(&value))
    });
    LegacyConfigMigrationPlan {
        id: CATEGORY_DEEP_SPLIT_MIGRATION_ID.to_string(),
        inspect: Rc::clone(&transform),
        mode: MigrationMode::ReplaceTarget,
        should_run: Some(Rc::new(has_legacy_category_names as fn(&Value) -> bool)),
        sources: Vec::new(),
        target_path,
        transform,
    }
}

// Gated on content like the category plan: a config that never named the legacy harness block is
// left untouched - no backup, no journal, no `_migrations` marker.
fn harness_native_rename_plan(target_path: String) -> LegacyConfigMigrationPlan {
    let transform: LegacyConfigMigrationTransform = Rc::new(|loaded| {
        let value = loaded
            .first()
            .map(|source| source.value.clone())
            .unwrap_or(Value::Null);
        Ok(transform_harness_native_rename(&value))
    });
    LegacyConfigMigrationPlan {
        id: HARNESS_NATIVE_RENAME_MIGRATION_ID.to_string(),
        inspect: Rc::clone(&transform),
        mode: MigrationMode::ReplaceTarget,
        should_run: Some(Rc::new(has_legacy_harness_blocks as fn(&Value) -> bool)),
        sources: Vec::new(),
        target_path,
        transform,
    }
}

fn subscription_provider_rename_plan(target_path: String) -> LegacyConfigMigrationPlan {
    let transform: LegacyConfigMigrationTransform = Rc::new(|loaded| {
        let value = loaded
            .first()
            .map(|source| source.value.clone())
            .unwrap_or(Value::Null);
        Ok(transform_subscription_provider_rename(&value))
    });
    LegacyConfigMigrationPlan {
        id: SUBSCRIPTION_PROVIDER_RENAME_MIGRATION_ID.to_string(),
        inspect: Rc::clone(&transform),
        mode: MigrationMode::ReplaceTarget,
        should_run: Some(Rc::new(
            has_legacy_subscription_provider_ids as fn(&Value) -> bool,
        )),
        sources: Vec::new(),
        target_path,
        transform,
    }
}

fn existing_omo_config_path(
    directory: &str,
    options: &ConfigMigrationDiscoveryOptions<'_>,
) -> Result<Option<String>, DiscoveryFsError> {
    let file_system = discovery_file_system(options);
    for file_name in ["omo.jsonc", "omo.json"] {
        let path = options.path_operations.join(&[directory, file_name]);
        if file_system.exists(&path) {
            return canonical_path(&path, options).map(Some);
        }
    }
    Ok(None)
}

/// Plans every legacy config migration: OpenCode user/project unification, `config.jsonc`
/// folding, and the reasoning-unification rewrite of each existing or planned omo target.
pub fn create_legacy_config_migration_plans(
    options: &CreateLegacyConfigMigrationPlansOptions<'_>,
) -> Result<Vec<LegacyConfigMigrationPlan>, DiscoveryFsError> {
    let discovery = &options.discovery;
    let timestamp = backup_timestamp(options.backup_timestamp);
    let mut groups = discover_legacy_config_groups(discovery)?.into_iter();
    let (Some(open_code_group), Some(config_jsonc_group)) = (groups.next(), groups.next()) else {
        return Ok(Vec::new());
    };
    let path_operations = host_path_operations(discovery);
    let user_target_path = path_operations.join(&[discovery.home_dir, ".maho", "omo.jsonc"]);

    let mut user_sources = Vec::new();
    let mut project_sources: Vec<(String, Vec<DiscoveredLegacyConfigSource>)> = Vec::new();
    for source in open_code_group.sources {
        match source.project_root.clone() {
            None => user_sources.push(source),
            Some(project_root) => {
                match project_sources
                    .iter_mut()
                    .find(|(root, _)| *root == project_root)
                {
                    Some((_, sources)) => sources.push(source),
                    None => project_sources.push((project_root, vec![source])),
                }
            }
        }
    }
    let mut open_code_inputs = Vec::new();
    if !user_sources.is_empty() {
        open_code_inputs.push(OpenCodePlanInput {
            scopes: vec![OpenCodeTransformScope::User],
            sources: user_sources,
            target_path: user_target_path.clone(),
        });
    }
    for (project_root, sources) in project_sources {
        open_code_inputs.push(OpenCodePlanInput {
            target_path: path_operations.join(&[&project_root, ".omo", "omo.jsonc"]),
            scopes: vec![OpenCodeTransformScope::Project { project_root }],
            sources,
        });
    }
    let mut plans = Vec::new();
    for input in merge_open_code_inputs(open_code_inputs, discovery)? {
        plans.push(open_code_plan(input, discovery, &timestamp)?);
    }
    if config_jsonc_group.id == CONFIG_JSONC_MIGRATION_ID && !config_jsonc_group.sources.is_empty()
    {
        plans.push(config_jsonc_plan(
            config_jsonc_group.sources,
            discovery,
            &timestamp,
            user_target_path,
        )?);
    }

    let mut reasoning_targets: Vec<(String, String)> = Vec::new();
    let mut add_reasoning_target = |target_path: Option<String>| -> Result<(), DiscoveryFsError> {
        let Some(target_path) = target_path else {
            return Ok(());
        };
        let key = path_key(&target_path, discovery)?;
        match reasoning_targets
            .iter_mut()
            .find(|(existing, _)| *existing == key)
        {
            Some(entry) => entry.1 = target_path,
            None => reasoning_targets.push((key, target_path)),
        }
        Ok(())
    };
    let user_omo_directory = discovery
        .path_operations
        .join(&[discovery.home_dir, ".maho"]);
    add_reasoning_target(existing_omo_config_path(&user_omo_directory, discovery)?)?;
    for project_root in project_directories(discovery)? {
        let directory = discovery.path_operations.join(&[&project_root, ".omo"]);
        add_reasoning_target(existing_omo_config_path(&directory, discovery)?)?;
    }
    for plan in &plans {
        add_reasoning_target(Some(plan.target_path.clone()))?;
    }

    let in_place_targets: Vec<String> = reasoning_targets
        .into_iter()
        .map(|(_, target_path)| target_path)
        .collect();
    plans.extend(in_place_targets.iter().cloned().map(reasoning_plan));
    plans.extend(in_place_targets.iter().cloned().map(category_deep_split_plan));
    plans.extend(in_place_targets.iter().cloned().map(harness_native_rename_plan));
    plans.extend(
        in_place_targets
            .into_iter()
            .map(subscription_provider_rename_plan),
    );
    Ok(plans)
}

#[cfg(test)]
mod tests {
    use super::encode_uri_component;

    #[test]
    fn encode_uri_component_matches_javascript_reserved_set() {
        // given
        let path = "/tmp/a b/é(x)!";

        // when
        let encoded = encode_uri_component(path);

        // then
        assert_eq!(encoded, "%2Ftmp%2Fa%20b%2F%C3%A9(x)!");
    }
}
