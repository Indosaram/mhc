use std::collections::HashSet;

use crate::discovery_paths::{
    canonical_path, config_paths, discovery_file_system, path_key, profile_directories,
    project_directories,
};
use crate::discovery_roots::config_roots;
use crate::types::{
    ConfigMigrationDiscoveryOptions, DiscoveredLegacyConfigSource, DiscoveryFsError,
    LegacyConfigMigrationGroup, LegacyConfigSourceKind, PathOperations,
};

pub const OPENCODE_CONFIG_MIGRATION_ID: &str = "2026-07-opencode-config-unification";
pub const CONFIG_JSONC_MIGRATION_ID: &str = "2026-07-codex-config-jsonc";

struct SourceSeed {
    base_root: Option<String>,
    config_path: String,
    is_active_profile: bool,
    precedence: i64,
    profile: Option<String>,
    project_root: Option<String>,
}

struct Collector<'o, 'a> {
    options: &'o ConfigMigrationDiscoveryOptions<'a>,
    seen: HashSet<String>,
    sources: Vec<DiscoveredLegacyConfigSource>,
}

impl Collector<'_, '_> {
    fn add_config_and_sidecar(&mut self, seed: SourceSeed) -> Result<(), DiscoveryFsError> {
        let options = self.options;
        let file_system = discovery_file_system(options);
        let config_path = canonical_path(&seed.config_path, options)?;
        let config_key = path_key(&config_path, options)?;
        let source = |kind, path| DiscoveredLegacyConfigSource {
            base_root: seed.base_root.clone(),
            config_path: config_path.clone(),
            is_active_profile: seed.is_active_profile,
            kind,
            path,
            precedence: seed.precedence,
            profile: seed.profile.clone(),
            project_root: seed.project_root.clone(),
        };
        if !self.seen.contains(&config_key) && file_system.exists(&seed.config_path) {
            self.seen.insert(config_key);
            let kind = match (&seed.project_root, &seed.profile) {
                (Some(_), _) => LegacyConfigSourceKind::ProjectConfig,
                (None, None) => LegacyConfigSourceKind::UserConfig,
                (None, Some(_)) => LegacyConfigSourceKind::ProfileConfig,
            };
            self.sources.push(source(kind, config_path.clone()));
        }
        let sidecar_path = format!("{}.migrations.json", seed.config_path);
        let sidecar_key = path_key(&sidecar_path, options)?;
        if !self.seen.contains(&sidecar_key) && file_system.exists(&sidecar_path) {
            self.seen.insert(sidecar_key);
            let path = canonical_path(&sidecar_path, options)?;
            self.sources
                .push(source(LegacyConfigSourceKind::MigrationSidecar, path));
        }
        Ok(())
    }
}

fn discover_open_code_sources(
    options: &ConfigMigrationDiscoveryOptions<'_>,
) -> Result<Vec<DiscoveredLegacyConfigSource>, DiscoveryFsError> {
    let mut collector = Collector {
        options,
        seen: HashSet::new(),
        sources: Vec::new(),
    };
    let path_operations = options.path_operations;
    for root in config_roots(options)? {
        for (index, config_path) in (0..).zip(config_paths(&root.path, options)) {
            collector.add_config_and_sidecar(SourceSeed {
                base_root: Some(root.path.clone()),
                config_path,
                is_active_profile: false,
                precedence: root.precedence * 10 + index,
                profile: None,
                project_root: None,
            })?;
        }
        for profile in profile_directories(&root.path, options)? {
            let profile_directory = path_operations.join(&[&root.path, "profiles", &profile]);
            let is_active_profile = root.active_profile.as_ref().is_some_and(|active| {
                *active == profile
                    || (path_operations == PathOperations::Win32
                        && active.to_lowercase() == profile.to_lowercase())
            });
            for (index, config_path) in (0..).zip(config_paths(&profile_directory, options)) {
                collector.add_config_and_sidecar(SourceSeed {
                    base_root: Some(root.path.clone()),
                    config_path,
                    is_active_profile,
                    precedence: root.precedence * 10 + index,
                    profile: Some(profile.clone()),
                    project_root: None,
                })?;
            }
        }
    }
    for (directory_index, project_root) in (0..).zip(project_directories(options)?) {
        let config_directory = path_operations.join(&[&project_root, ".opencode"]);
        for (file_index, config_path) in (0..).zip(config_paths(&config_directory, options)) {
            collector.add_config_and_sidecar(SourceSeed {
                base_root: None,
                config_path,
                is_active_profile: false,
                precedence: 1000 + directory_index * 10 + file_index,
                profile: None,
                project_root: Some(project_root.clone()),
            })?;
        }
    }
    Ok(collector.sources)
}

fn discover_config_jsonc_sources(
    options: &ConfigMigrationDiscoveryOptions<'_>,
) -> Result<Vec<DiscoveredLegacyConfigSource>, DiscoveryFsError> {
    let config_path = options
        .path_operations
        .join(&[options.home_dir, ".maho", "config.jsonc"]);
    let file_system = discovery_file_system(options);
    let source = |kind, path| DiscoveredLegacyConfigSource {
        base_root: None,
        config_path: String::new(),
        is_active_profile: false,
        kind,
        path,
        precedence: 0,
        profile: None,
        project_root: None,
    };
    let mut sources = Vec::new();
    if file_system.exists(&config_path) {
        let canonical = canonical_path(&config_path, options)?;
        sources.push(DiscoveredLegacyConfigSource {
            config_path: canonical.clone(),
            ..source(LegacyConfigSourceKind::ConfigJsonc, canonical)
        });
    }
    let sidecar_path = format!("{config_path}.migrations.json");
    if file_system.exists(&sidecar_path) {
        sources.push(DiscoveredLegacyConfigSource {
            config_path: canonical_path(&config_path, options)?,
            ..source(
                LegacyConfigSourceKind::MigrationSidecar,
                canonical_path(&sidecar_path, options)?,
            )
        });
    }
    Ok(sources)
}

pub fn discover_legacy_config_groups(
    options: &ConfigMigrationDiscoveryOptions<'_>,
) -> Result<Vec<LegacyConfigMigrationGroup>, DiscoveryFsError> {
    Ok(vec![
        LegacyConfigMigrationGroup {
            id: OPENCODE_CONFIG_MIGRATION_ID.to_string(),
            sources: discover_open_code_sources(options)?,
        },
        LegacyConfigMigrationGroup {
            id: CONFIG_JSONC_MIGRATION_ID.to_string(),
            sources: discover_config_jsonc_sources(options)?,
        },
    ])
}
