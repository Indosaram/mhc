use std::collections::HashSet;

use serde_json::{Map, Value};
use utils::migration::{
    HookRename, agent_name_for, hook_name_mapping, migrate_agent_names, migrate_model_versions,
};

use crate::deep_diff::deep_difference;
use crate::legacy_history::legacy_migration_history;
use crate::record_values::{merge_records, without_legacy_metadata};
use crate::schema_url::OMO_SCHEMA_URL;
use crate::transform_types::{
    ConfigMigrationTransformResult, LoadedLegacyConfigSource, OpenCodeTransformScope,
    TransformOpenCodeSourcesInput,
};
use crate::types::{DiscoveredLegacyConfigSource, LegacyConfigSourceKind};

struct LoadedConfig<'a> {
    metadata: &'a DiscoveredLegacyConfigSource,
    value: Map<String, Value>,
}

fn replace_disabled_agents(value: &Value) -> Value {
    let Value::Array(entries) = value else {
        return value.clone();
    };
    Value::Array(
        entries
            .iter()
            .map(|entry| match entry {
                Value::String(name) => Value::String(agent_name_for(name)),
                other => other.clone(),
            })
            .collect(),
    )
}

fn replace_disabled_hooks(value: &Value) -> Value {
    let Value::Array(entries) = value else {
        return value.clone();
    };
    Value::Array(
        entries
            .iter()
            .filter_map(|entry| match entry {
                Value::String(name) => match hook_name_mapping(name) {
                    Some(HookRename::Removed) => None,
                    Some(HookRename::Renamed(renamed)) => Some(Value::String(renamed.to_string())),
                    None => Some(entry.clone()),
                },
                other => Some(other.clone()),
            })
            .collect(),
    )
}

fn transform_legacy_keys(value: &Map<String, Value>, history: &[String]) -> Map<String, Value> {
    let mut config = without_legacy_metadata(value);
    let applied: HashSet<String> = history.iter().cloned().collect();
    if let Some(Value::Object(agents)) = config.get("agents") {
        let named = migrate_agent_names(agents).migrated;
        let migrated = migrate_model_versions(&named, Some(&applied)).migrated;
        config.insert("agents".into(), Value::Object(migrated));
    }
    if let Some(Value::Object(categories)) = config.get("categories") {
        let migrated = migrate_model_versions(categories, Some(&applied)).migrated;
        config.insert("categories".into(), Value::Object(migrated));
    }
    if let Some(omo_agent) = config.shift_remove("omo_agent") {
        config.insert("sisyphus_agent".into(), omo_agent);
    }
    config.shift_remove("lsp");
    let hashline = match config.get_mut("experimental") {
        Some(Value::Object(experimental)) => experimental
            .shift_remove("hashline_edit")
            .map(|hashline| (hashline, experimental.is_empty())),
        _ => None,
    };
    if let Some((hashline, experimental_empty)) = hashline {
        if !config.contains_key("hashline_edit") {
            config.insert("hashline_edit".into(), hashline);
        }
        if experimental_empty {
            config.shift_remove("experimental");
        }
    }
    if let Some(disabled) = config.get("disabled_agents") {
        let replaced = replace_disabled_agents(disabled);
        config.insert("disabled_agents".into(), replaced);
    }
    if let Some(disabled) = config.get("disabled_hooks") {
        let replaced = replace_disabled_hooks(disabled);
        config.insert("disabled_hooks".into(), replaced);
    }
    config
}

fn history_strings(history: &Map<String, Value>, config_path: &str) -> Vec<String> {
    history
        .get(config_path)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn loaded_configs<'a>(
    discovered: &'a [DiscoveredLegacyConfigSource],
    sources: &[LoadedLegacyConfigSource],
) -> Vec<LoadedConfig<'a>> {
    let history_by_config_path = legacy_migration_history(discovered, sources);
    let mut configs = Vec::new();
    for source in sources {
        let Some(metadata) = discovered
            .iter()
            .rev()
            .find(|candidate| candidate.path == source.path)
        else {
            continue;
        };
        let Value::Object(value) = &source.value else {
            continue;
        };
        if metadata.kind == LegacyConfigSourceKind::MigrationSidecar {
            continue;
        }
        let history = history_strings(&history_by_config_path, &metadata.config_path);
        configs.push(LoadedConfig {
            metadata,
            value: transform_legacy_keys(value, &history),
        });
    }
    configs
}

fn merged_configs(configs: &[&LoadedConfig<'_>]) -> Map<String, Value> {
    let mut sorted = configs.to_vec();
    sorted.sort_by_key(|config| std::cmp::Reverse(config.metadata.precedence));
    sorted.into_iter().fold(Map::new(), |merged, config| {
        merge_records(&merged, &config.value)
    })
}

fn document_with_history(
    opencode: Map<String, Value>,
    profiles: Map<String, Value>,
    discovered: &[DiscoveredLegacyConfigSource],
    sources: &[LoadedLegacyConfigSource],
) -> ConfigMigrationTransformResult {
    let history = legacy_migration_history(discovered, sources);
    let mut document = Map::new();
    document.insert("$schema".into(), Value::String(OMO_SCHEMA_URL.into()));
    if !opencode.is_empty() {
        document.insert("[opencode]".into(), Value::Object(opencode));
    }
    if !profiles.is_empty() {
        document.insert("profiles".into(), Value::Object(profiles));
    }
    if !history.is_empty() {
        document.insert("legacy_migrations".into(), Value::Object(history));
    }
    ConfigMigrationTransformResult {
        diagnostics: Vec::new(),
        document,
    }
}

fn unique<'a>(values: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    let mut seen: Vec<&str> = Vec::new();
    for value in values {
        if !seen.contains(&value) {
            seen.push(value);
        }
    }
    seen
}

fn transform_user_sources(
    discovered: &[DiscoveredLegacyConfigSource],
    sources: &[LoadedLegacyConfigSource],
) -> ConfigMigrationTransformResult {
    let configs = loaded_configs(discovered, sources);
    let root_configs: Vec<&LoadedConfig<'_>> = configs
        .iter()
        .filter(|config| config.metadata.kind == LegacyConfigSourceKind::UserConfig)
        .collect();
    let roots = unique(
        root_configs
            .iter()
            .filter_map(|config| config.metadata.base_root.as_deref()),
    );
    let root_by_directory: Vec<(&str, Map<String, Value>)> = roots
        .into_iter()
        .map(|root| {
            let members: Vec<&LoadedConfig<'_>> = root_configs
                .iter()
                .copied()
                .filter(|config| config.metadata.base_root.as_deref() == Some(root))
                .collect();
            (root, merged_configs(&members))
        })
        .collect();
    let empty = Map::new();
    let mut profiles = Map::new();
    for profile in unique(
        configs
            .iter()
            .filter_map(|config| config.metadata.profile.as_deref()),
    ) {
        let differences: Vec<LoadedConfig<'_>> = configs
            .iter()
            .filter(|config| {
                config.metadata.kind == LegacyConfigSourceKind::ProfileConfig
                    && config.metadata.profile.as_deref() == Some(profile)
            })
            .map(|config| {
                let root = config.metadata.base_root.as_deref().unwrap_or_default();
                let base = root_by_directory
                    .iter()
                    .find(|(directory, _)| *directory == root)
                    .map_or(&empty, |(_, merged)| merged);
                LoadedConfig {
                    metadata: config.metadata,
                    value: deep_difference(base, &config.value),
                }
            })
            .collect();
        let difference = merged_configs(&differences.iter().collect::<Vec<_>>());
        if !difference.is_empty() {
            let mut entry = Map::new();
            entry.insert("[opencode]".into(), Value::Object(difference));
            profiles.insert(profile.to_string(), Value::Object(entry));
        }
    }
    let user_discovered: Vec<DiscoveredLegacyConfigSource> = discovered
        .iter()
        .filter(|source| source.project_root.is_none())
        .cloned()
        .collect();
    document_with_history(
        merged_configs(&root_configs),
        profiles,
        &user_discovered,
        sources,
    )
}

fn transform_project_sources(
    input: &TransformOpenCodeSourcesInput<'_>,
    project_root: &str,
) -> ConfigMigrationTransformResult {
    let discovered: Vec<DiscoveredLegacyConfigSource> = input
        .discovered
        .iter()
        .filter(|source| source.project_root.as_deref() == Some(project_root))
        .cloned()
        .collect();
    let configs = loaded_configs(&discovered, input.sources);
    let merged = merged_configs(&configs.iter().collect::<Vec<_>>());
    document_with_history(merged, Map::new(), &discovered, input.sources)
}

pub fn transform_open_code_sources(
    input: &TransformOpenCodeSourcesInput<'_>,
) -> ConfigMigrationTransformResult {
    match input.scope {
        OpenCodeTransformScope::User => transform_user_sources(input.discovered, input.sources),
        OpenCodeTransformScope::Project { project_root } => {
            transform_project_sources(input, project_root)
        }
    }
}
