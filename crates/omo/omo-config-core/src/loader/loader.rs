use serde_json::{Map, Value, json};

use crate::internal::jsonc::parse_jsonc_safe;
use crate::internal::validate::safe_parse;
use crate::issue::Issues;
use crate::loader::merge::merge_omo_config_records;
use crate::loader::paths::{ResolveOmoConfigPathsOptions, process_env, resolve_omo_config_paths};
use crate::loader::resolution::{
    ResolveOmoConfigViewOptions, resolve_omo_config_view, resolve_omo_profile_name,
};
use crate::loader::types::{
    DIAGNOSTIC_PARSE, DIAGNOSTIC_READ, DIAGNOSTIC_VALIDATION, LoadOmoConfigOptions,
    OmoConfigDiagnostic, OmoConfigRawLayer, OmoConfigReadFileSystem, OmoConfigSource,
    StdReadFileSystem,
};
use crate::schema::codegraph::omo_codegraph_settings_schema;
use crate::schema::config::{omo_config_layer_schema, omo_config_schema};
use crate::schema::task::resolve_omo_task_settings_default;

fn default_raw_config() -> Map<String, Value> {
    let codegraph = safe_parse(&omo_codegraph_settings_schema(), &json!({}))
        .expect("codegraph settings materialize every default");
    let task = resolve_omo_task_settings_default(&json!({}))
        .expect("task settings materialize every default");
    let mut config = Map::new();
    config.insert("agents".into(), Value::Object(Map::new()));
    config.insert("categories".into(), Value::Object(Map::new()));
    config.insert("codegraph".into(), codegraph);
    config.insert("task".into(), task);
    config.insert("teams".into(), Value::Object(Map::new()));
    config
}

fn strip_resolution_control_keys(config: Map<String, Value>) -> Map<String, Value> {
    let mut resolved = Map::new();
    for (key, value) in config {
        if key == "[codex]" || key == "[opencode]" || key == "[senpi]" || key == "profiles" {
            continue;
        }
        resolved.insert(key, value);
    }
    resolved
}

fn validation_diagnostic(path: &str, issues: &Issues) -> OmoConfigDiagnostic {
    let issue_paths: Vec<String> = issues.iter().map(|issue| issue.path_string()).collect();
    OmoConfigDiagnostic {
        kind: DIAGNOSTIC_VALIDATION,
        message: format!("Invalid omo config at {path}: {}", issue_paths.join(", ")),
        path: path.to_string(),
        issue_paths,
    }
}

fn to_record(value: &Value) -> Option<Map<String, Value>> {
    match value {
        Value::Object(map) => Some(map.clone()),
        _ => None,
    }
}

pub struct ReadConfigSource {
    pub diagnostic: Option<OmoConfigDiagnostic>,
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
            diagnostic: None,
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
                diagnostic: Some(OmoConfigDiagnostic {
                    kind: DIAGNOSTIC_READ,
                    message: format!("Failed to read {path}: {error}"),
                    path: path.to_string(),
                    issue_paths: Vec::new(),
                }),
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
            diagnostic: Some(OmoConfigDiagnostic {
                kind: DIAGNOSTIC_PARSE,
                message: format!("JSONC parse error in {path}: {detail}"),
                path: path.to_string(),
                issue_paths: Vec::new(),
            }),
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
    match safe_parse(&omo_config_layer_schema(), &data) {
        Err(issues) => {
            let diagnostic = validation_diagnostic(path, &issues);
            ReadConfigSource {
                diagnostic: Some(diagnostic),
                source: OmoConfigSource {
                    exists: true,
                    loaded: false,
                    path: path.to_string(),
                    scope,
                },
                value: None,
            }
        }
        Ok(_) => match to_record(&data) {
            None => ReadConfigSource {
                diagnostic: Some(OmoConfigDiagnostic {
                    kind: DIAGNOSTIC_VALIDATION,
                    message: format!("Invalid omo config at {path}: root must be an object"),
                    path: path.to_string(),
                    issue_paths: Vec::new(),
                }),
                source: OmoConfigSource {
                    exists: true,
                    loaded: false,
                    path: path.to_string(),
                    scope,
                },
                value: None,
            },
            Some(value) => ReadConfigSource {
                diagnostic: None,
                source: OmoConfigSource {
                    exists: true,
                    loaded: true,
                    path: path.to_string(),
                    scope,
                },
                value: Some(value),
            },
        },
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
        if let Some(diagnostic) = loaded.diagnostic {
            diagnostics.push(diagnostic);
        }
        if let Some(value) = loaded.value {
            layers.push(OmoConfigRawLayer {
                config: Value::Object(value.clone()),
                source: loaded.source,
            });
            merged = merge_omo_config_records(&merged, &value, None);
        }
    }

    let requested_profile =
        resolve_omo_profile_name(options.env.as_ref(), options.profile.as_ref());
    let resolved = resolve_omo_config_view(ResolveOmoConfigViewOptions {
        config: &merged,
        harness: options.harness.as_ref(),
        profile: requested_profile.as_ref(),
    });

    let with_defaults = merge_omo_config_records(&default_raw_config(), &resolved.config, None);
    let mut all_diagnostics = diagnostics;
    all_diagnostics.extend(resolved.diagnostics.clone());

    match safe_parse(&omo_config_schema(), &Value::Object(with_defaults)) {
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
            let fallback = default_raw_config();
            let fallback = match safe_parse(&omo_config_schema(), &Value::Object(fallback)) {
                Ok(Value::Object(map)) => map,
                _ => Map::new(),
            };
            all_diagnostics.push(validation_diagnostic("(merged omo config)", &issues));
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
