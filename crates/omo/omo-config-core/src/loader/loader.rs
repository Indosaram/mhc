use serde_json::{Map, Value, json};

use crate::internal::jsonc::parse_jsonc_safe;
use crate::internal::validate::safe_parse;
use crate::loader::layer_validation::{
    OmoConfigLayerValidation, invalid_value_diagnostics, validate_config_layer,
    validation_diagnostic,
};
use crate::loader::merge::merge_omo_config_records;
use crate::loader::paths::{ResolveOmoConfigPathsOptions, process_env, resolve_omo_config_paths};
use crate::loader::prune_invalid_leaves::{
    MAX_PRUNE_PASSES, PruneResult, prune_invalid_config_paths,
};
use crate::loader::resolution::{
    ResolveOmoConfigViewOptions, resolve_omo_config_view, resolve_omo_profile_name,
};
use crate::loader::types::{
    DIAGNOSTIC_DEPRECATED_KEYS, DIAGNOSTIC_PARSE, DIAGNOSTIC_READ, LoadOmoConfigOptions,
    MERGED_OMO_CONFIG_PATH, OmoConfigDiagnostic, OmoConfigRawLayer, OmoConfigReadFileSystem,
    OmoConfigSource, StdReadFileSystem,
};
use crate::schema::config::omo_config_schema;
use crate::schema::legacy_category_names::{
    LegacyCategoryRename, canonicalize_legacy_category_names,
};
use crate::schema::legacy_harness_names::{
    LegacyHarnessRename, canonicalize_legacy_harness_blocks,
};
use crate::schema::task::resolve_omo_task_settings_default;

fn default_raw_config() -> Map<String, Value> {
    let task = resolve_omo_task_settings_default(&json!({}))
        .expect("task settings materialize every default");
    let mut config = Map::new();
    config.insert("agents".into(), Value::Object(Map::new()));
    config.insert("categories".into(), Value::Object(Map::new()));
    config.insert("task".into(), task);
    config.insert("teams".into(), Value::Object(Map::new()));
    config
}

fn strip_resolution_control_keys(config: Map<String, Value>) -> Map<String, Value> {
    let mut resolved = Map::new();
    for (key, value) in config {
        if key == "profiles" || crate::loader::resolution::HARNESS_KEYS.contains(&key.as_str()) {
            continue;
        }
        resolved.insert(key, value);
    }
    resolved
}

pub struct ReadConfigSource {
    pub diagnostics: Vec<OmoConfigDiagnostic>,
    pub source: OmoConfigSource,
    pub value: Option<Map<String, Value>>,
}

fn read_config_source(
    path: &str,
    scope: &'static str,
    file_system: &dyn OmoConfigReadFileSystem,
) -> ReadConfigSource {
    if !file_system.exists(path) {
        return ReadConfigSource {
            diagnostics: Vec::new(),
            source: OmoConfigSource {
                exists: false,
                loaded: false,
                path: path.to_string(),
                scope,
            },
            value: None,
        };
    }

    let content = match file_system.read(path) {
        Ok(content) => content,
        Err(error) => {
            return ReadConfigSource {
                diagnostics: vec![OmoConfigDiagnostic {
                    kind: DIAGNOSTIC_READ,
                    message: format!("Failed to read {path}: {error}"),
                    path: path.to_string(),
                    issue_paths: Vec::new(),
                }],
                source: OmoConfigSource {
                    exists: true,
                    loaded: false,
                    path: path.to_string(),
                    scope,
                },
                value: None,
            };
        }
    };

    let parsed = parse_jsonc_safe(&content);
    if !parsed.errors.is_empty() {
        let detail = parsed
            .errors
            .iter()
            .map(|error| error.message.clone())
            .collect::<Vec<_>>()
            .join(", ");
        return ReadConfigSource {
            diagnostics: vec![OmoConfigDiagnostic {
                kind: DIAGNOSTIC_PARSE,
                message: format!("JSONC parse error in {path}: {detail}"),
                path: path.to_string(),
                issue_paths: Vec::new(),
            }],
            source: OmoConfigSource {
                exists: true,
                loaded: false,
                path: path.to_string(),
                scope,
            },
            value: None,
        };
    }

    let data = parsed.data.unwrap_or(Value::Null);
    match validate_config_layer(path, &data) {
        OmoConfigLayerValidation::Loaded { diagnostics, value } => ReadConfigSource {
            diagnostics,
            source: OmoConfigSource {
                exists: true,
                loaded: true,
                path: path.to_string(),
                scope,
            },
            value: Some(value),
        },
        OmoConfigLayerValidation::NotLoaded { diagnostics } => ReadConfigSource {
            diagnostics,
            source: OmoConfigSource {
                exists: true,
                loaded: false,
                path: path.to_string(),
                scope,
            },
            value: None,
        },
    }
}

fn legacy_rename_detail(dropped: bool, path: &str, canonical: &str) -> String {
    if dropped {
        format!("{path} ignored because {canonical} is also configured")
    } else {
        format!("{path} renamed to {canonical}")
    }
}

fn legacy_category_diagnostic(path: &str, renames: &[LegacyCategoryRename]) -> OmoConfigDiagnostic {
    let detail = renames
        .iter()
        .map(|rename| legacy_rename_detail(rename.dropped, &rename.path, &rename.canonical))
        .collect::<Vec<_>>()
        .join(", ");
    OmoConfigDiagnostic {
        kind: DIAGNOSTIC_DEPRECATED_KEYS,
        message: format!(
            "Deprecated category name in {path}: {detail}. Rename it; the alias is removed in a future release."
        ),
        path: path.to_string(),
        issue_paths: renames.iter().map(|rename| rename.path.clone()).collect(),
    }
}

fn legacy_harness_diagnostic(path: &str, renames: &[LegacyHarnessRename]) -> OmoConfigDiagnostic {
    let detail = renames
        .iter()
        .map(|rename| legacy_rename_detail(rename.dropped, &rename.path, &rename.canonical))
        .collect::<Vec<_>>()
        .join(", ");
    OmoConfigDiagnostic {
        kind: DIAGNOSTIC_DEPRECATED_KEYS,
        message: format!(
            "Deprecated harness block in {path}: {detail}. Rename it; the alias is removed in a future release."
        ),
        path: path.to_string(),
        issue_paths: renames.iter().map(|rename| rename.path.clone()).collect(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadOmoConfigResult {
    pub config: Map<String, Value>,
    pub diagnostics: Vec<OmoConfigDiagnostic>,
    pub layers: Vec<OmoConfigRawLayer>,
    pub profile: Option<String>,
    pub sources: Vec<OmoConfigSource>,
}

pub fn load_omo_config(options: &LoadOmoConfigOptions<'_>) -> LoadOmoConfigResult {
    let std_file_system = StdReadFileSystem;
    let file_system: &dyn OmoConfigReadFileSystem = options.file_system.unwrap_or(&std_file_system);
    let cwd = options.cwd.clone().unwrap_or_else(|| {
        crate::internal::posix_path::to_posix_path(
            &std::env::current_dir()
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_default(),
        )
    });
    let mut merged: Map<String, Value> = Map::new();
    let mut diagnostics: Vec<OmoConfigDiagnostic> = Vec::new();
    let mut layers: Vec<OmoConfigRawLayer> = Vec::new();
    let mut sources: Vec<OmoConfigSource> = Vec::new();

    let candidates = resolve_omo_config_paths(&ResolveOmoConfigPathsOptions {
        cwd,
        env: options.env.clone(),
        file_system: options.file_system,
        platform: options.platform.clone(),
    });

    for candidate in candidates {
        let loaded = read_config_source(&candidate.path, candidate.scope, file_system);
        sources.push(loaded.source.clone());
        diagnostics.extend(loaded.diagnostics);
        if let Some(value) = loaded.value {
            // A retired category key or harness block still resolves, so a config the startup migration
            // could not rewrite (locked run, read-only project file) keeps applying its override instead
            // of being ignored.
            let canonicalized = canonicalize_legacy_category_names(&Value::Object(value.clone()));
            if !canonicalized.renames.is_empty() {
                diagnostics.push(legacy_category_diagnostic(
                    &candidate.path,
                    &canonicalized.renames,
                ));
            }
            let harness_canonicalized =
                canonicalize_legacy_harness_blocks(&Value::Object(canonicalized.document.clone()));
            if !harness_canonicalized.renames.is_empty() {
                diagnostics.push(legacy_harness_diagnostic(
                    &candidate.path,
                    &harness_canonicalized.renames,
                ));
            }
            let canonical_document = harness_canonicalized.document;
            layers.push(OmoConfigRawLayer {
                config: Value::Object(canonical_document.clone()),
                source: loaded.source,
            });
            merged = merge_omo_config_records(&merged, &canonical_document);
        }
    }

    let requested_profile =
        resolve_omo_profile_name(options.env.as_ref(), options.profile.as_ref());
    let resolved = resolve_omo_config_view(ResolveOmoConfigViewOptions {
        config: &merged,
        harness: options.harness.as_ref(),
        profile: requested_profile.as_ref(),
    });

    let with_defaults = merge_omo_config_records(&default_raw_config(), &resolved.config);
    let mut all_diagnostics = diagnostics;
    all_diagnostics.extend(resolved.diagnostics.clone());

    match safe_parse(&omo_config_schema(), &Value::Object(with_defaults.clone())) {
        Ok(validated) => {
            let config = match validated {
                Value::Object(map) => map,
                _ => Map::new(),
            };
            LoadOmoConfigResult {
                config: strip_resolution_control_keys(config),
                diagnostics: all_diagnostics,
                layers,
                profile: resolved.profile,
                sources,
            }
        }
        Err(issues) => {
            // Every layer already validated on its own; a merged value that still fails the full
            // schema (a partial team spec, say) is dropped like any other invalid value instead of
            // resetting the config.
            let validate = |record: &Map<String, Value>| {
                safe_parse(&omo_config_schema(), &Value::Object(record.clone())).map(|_| ())
            };
            match prune_invalid_config_paths(&with_defaults, &issues, &validate, MAX_PRUNE_PASSES) {
                PruneResult::Ok { config, dropped } => {
                    let validated = safe_parse(&omo_config_schema(), &Value::Object(config))
                        .unwrap_or_else(|_| Value::Object(with_defaults.clone()));
                    let config = match validated {
                        Value::Object(map) => map,
                        _ => Map::new(),
                    };
                    all_diagnostics
                        .extend(invalid_value_diagnostics(MERGED_OMO_CONFIG_PATH, &dropped));
                    LoadOmoConfigResult {
                        config: strip_resolution_control_keys(config),
                        diagnostics: all_diagnostics,
                        layers,
                        profile: resolved.profile,
                        sources,
                    }
                }
                PruneResult::NotOk { .. } => {
                    let fallback = default_raw_config();
                    let fallback = match safe_parse(&omo_config_schema(), &Value::Object(fallback)) {
                        Ok(Value::Object(map)) => map,
                        _ => Map::new(),
                    };
                    all_diagnostics.push(validation_diagnostic(MERGED_OMO_CONFIG_PATH, &issues));
                    LoadOmoConfigResult {
                        config: strip_resolution_control_keys(fallback),
                        diagnostics: all_diagnostics,
                        layers,
                        profile: resolved.profile,
                        sources,
                    }
                }
            }
        }
    }
}

pub fn default_load_options<'a>() -> LoadOmoConfigOptions<'a> {
    LoadOmoConfigOptions {
        cwd: None,
        env: Some(process_env()),
        file_system: None,
        harness: None,
        platform: None,
        profile: None,
    }
}
