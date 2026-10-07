use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use crate::internal::jsonc::parse_jsonc_safe;
use crate::internal::plain_object::is_plain_object;
use crate::internal::posix_path::{posix_dirname, posix_join, posix_resolve, to_posix_path};
use crate::internal::validate::safe_parse;
use crate::issue::PathSegment;
use crate::loader::paths::resolve_home_dir;
use crate::migration::merge::{collect_migration_edits, merge_without_clobber};
use crate::migration::predicate::has_migration_marker;
use crate::migration::types::{
    MigrationEnvironment, MigrationError, MigrationFileSystem, MigrationTargetWriter,
    MigrationTargetWriterInput,
};
use crate::schema::config::omo_config_schema;
use crate::schema::harness::{
    OMO_CONFIG_HARNESS_IDS, OMO_CONFIG_LEGACY_HARNESS_IDS, harness_block_key,
};
use crate::writer::types::{OmoConfigEdit, UpdateOmoConfigOptions};
use crate::writer::writer::update_omo_config;

pub fn parse_document(path: &str, content: &str) -> Result<Value, MigrationError> {
    let parsed = parse_jsonc_safe(content);
    if !parsed.errors.is_empty()
        || parsed.data.is_none()
        || !is_plain_object(parsed.data.as_ref().unwrap())
    {
        let detail = parsed
            .errors
            .iter()
            .map(|error| format!("{} at {}", error.message, error.offset))
            .collect::<Vec<_>>()
            .join(", ");
        let suffix = if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        };
        return Err(MigrationError::transaction(format!(
            "Migration document at {path} is not a JSONC object{suffix}"
        )));
    }
    Ok(parsed.data.unwrap())
}

pub fn target_document(
    path: &str,
    file_system: &dyn MigrationFileSystem,
) -> Result<Value, MigrationError> {
    if !file_system.exists(path) {
        return Ok(Value::Object(Map::new()));
    }
    let content = file_system.read(path)?;
    parse_document(path, &content)
}

fn marker_value(
    target: &Value,
    migration_id: &str,
    target_path: &str,
) -> Result<Vec<String>, MigrationError> {
    let value = target.get("_migrations");
    let value = match value {
        None => return Ok(vec![migration_id.to_string()]),
        Some(v) => v,
    };
    let arr = match value.as_array() {
        Some(a) => a,
        None => {
            return Err(MigrationError::validation(
                target_path,
                "the existing migration marker must be an array of strings",
            ));
        }
    };
    let mut markers = Vec::new();
    for item in arr {
        match item.as_str() {
            Some(s) => markers.push(s.to_string()),
            None => {
                return Err(MigrationError::validation(
                    target_path,
                    "the existing migration marker must be an array of strings",
                ));
            }
        }
    }
    if has_migration_marker(target, migration_id) {
        Ok(markers)
    } else {
        markers.push(migration_id.to_string());
        Ok(markers)
    }
}

pub fn validate_target(target_path: &str, document: &Value) -> Result<(), MigrationError> {
    let schema = omo_config_schema();
    match safe_parse(&schema, document) {
        Ok(_) => Ok(()),
        Err(issues) => {
            let detail = issues
                .iter()
                .map(|issue| format!("{}: {}", issue.path.join("."), issue.message))
                .collect::<Vec<_>>()
                .join(", ");
            Err(MigrationError::validation(target_path, detail))
        }
    }
}

struct WriterInput {
    project_dir: Option<String>,
    scope: &'static str,
}

fn writer_input(
    target_path: &str,
    env: &MigrationEnvironment,
) -> Result<WriterInput, MigrationError> {
    let home_dir = resolve_home_dir(env);
    let user_directory = to_posix_path(&posix_join(&[&home_dir, ".maho"]));
    let normalized_target = to_posix_path(target_path);
    let dir_name = posix_dirname(&normalized_target);
    let file_name = normalized_target.rsplit('/').next().unwrap_or("");

    // maho: the planner canonicalizes the home directory (e.g. macOS /var -> /private/var) while
    // HOME may not be, so also accept a canonical match. With the COMP `.omo` home this case fell
    // through to the project `.omo` matcher below; the `.maho` home has no such fallback.
    let is_user_directory = dir_name == user_directory
        || matches!(
            (std::fs::canonicalize(&dir_name), std::fs::canonicalize(&user_directory)),
            (Ok(a), Ok(b)) if a == b
        );
    if is_user_directory && (file_name == "omo.json" || file_name == "omo.jsonc") {
        return Ok(WriterInput {
            project_dir: None,
            scope: "user",
        });
    }

    let parent_base = dir_name.rsplit('/').next().unwrap_or("");
    if parent_base == ".omo" && (file_name == "omo.json" || file_name == "omo.jsonc") {
        let project_dir = posix_dirname(&dir_name);
        return Ok(WriterInput {
            project_dir: Some(project_dir),
            scope: "project",
        });
    }

    Err(MigrationError::transaction(format!(
        "Migration target is not an omo config path: {target_path}"
    )))
}

fn same_resolved_path(a: &str, b: &str) -> bool {
    to_posix_path(&posix_resolve(&[a])) == to_posix_path(&posix_resolve(&[b]))
}

pub fn write_omo_migration_target(
    input: MigrationTargetWriterInput<'_>,
) -> Result<(), MigrationError> {
    let options = writer_input(input.target_path, input.env)?;
    let update_opts = UpdateOmoConfigOptions {
        edits: input.edits.to_vec(),
        env: Some(input.env.clone()),
        file_system: Some(input.file_system),
        platform: None,
        project_dir: options.project_dir,
        scope: options.scope,
        target_path: Some(input.target_path.to_string()),
        timestamp: None,
    };
    let result = update_omo_config(&update_opts)
        .map_err(|error| MigrationError::transaction(error.to_string()))?;
    if !same_resolved_path(&result.path, input.target_path) {
        return Err(MigrationError::transaction(format!(
            "Migration writer resolved {} instead of {}",
            result.path, input.target_path
        )));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct PreparedTargetWrite {
    pub diagnostics: Vec<String>,
    pub document: Value,
    pub edits: Vec<OmoConfigEdit>,
}

/// Every harness block a config may carry, canonical and legacy, derived from the schema so this
/// list cannot drift. The legacy `[senpi]` block is renamed to `[native]` by a replace-target
/// migration whose output must be stripped too, otherwise a `[senpi].codegraph` leftover survives
/// the rename as `[native].codegraph` and strict validation still rejects the file.
fn omo_harness_blocks() -> Vec<String> {
    OMO_CONFIG_HARNESS_IDS
        .iter()
        .chain(OMO_CONFIG_LEGACY_HARNESS_IDS.iter())
        .map(|harness| harness_block_key(harness))
        .collect()
}

#[derive(Debug, Clone)]
struct RetiredCodegraphCleanup {
    pub diagnostics: Vec<String>,
    pub document: Value,
    pub edits: Vec<OmoConfigEdit>,
}

fn strip_codegraph_at(container: &mut Value, path: &[String], removed: &mut Vec<Vec<String>>) {
    let Some(map) = container.as_object_mut() else {
        return;
    };
    if map.remove("codegraph").is_some() {
        let mut next = path.to_vec();
        next.push("codegraph".to_string());
        removed.push(next);
    }
}

fn strip_retired_codegraph(document: &Value) -> RetiredCodegraphCleanup {
    let mut stripped = document.clone();
    let mut removed_paths: Vec<Vec<String>> = Vec::new();
    let harness_blocks = omo_harness_blocks();

    strip_codegraph_at(&mut stripped, &[], &mut removed_paths);
    for harness in &harness_blocks {
        if let Some(block) = stripped.get_mut(harness) {
            strip_codegraph_at(block, std::slice::from_ref(harness), &mut removed_paths);
        }
    }
    if let Some(profiles) = stripped.get_mut("profiles").and_then(Value::as_object_mut) {
        let names: Vec<String> = profiles.keys().cloned().collect();
        for name in names {
            let Some(profile) = profiles.get_mut(&name) else {
                continue;
            };
            let profile_path = vec!["profiles".to_string(), name.clone()];
            strip_codegraph_at(profile, &profile_path, &mut removed_paths);
            if !profile.is_object() {
                continue;
            }
            for harness in &harness_blocks {
                let mut block_path = profile_path.clone();
                block_path.push(harness.clone());
                if let Some(block) = profile.get_mut(harness) {
                    strip_codegraph_at(block, &block_path, &mut removed_paths);
                }
            }
        }
    }

    RetiredCodegraphCleanup {
        diagnostics: removed_paths
            .iter()
            .map(|path| {
                let joined = if path.is_empty() {
                    "codegraph".to_string()
                } else {
                    path.join(".")
                };
                format!("removed: {joined} (retired configuration)")
            })
            .collect(),
        edits: removed_paths
            .iter()
            .map(|path| OmoConfigEdit {
                path: path.iter().map(PathSegment::key).collect(),
                value: None,
            })
            .collect(),
        document: stripped,
    }
}

/// Target and transform output are stripped independently; a replace-target transform that passes the
/// target through reports the same paths twice without this.
fn unique_diagnostics(diagnostics: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut unique = Vec::new();
    for diagnostic in diagnostics {
        if seen.insert(diagnostic.clone()) {
            unique.push(diagnostic.clone());
        }
    }
    unique
}

pub fn prepare_target_write(
    additions: &Value,
    migration_id: &str,
    target: &Value,
    target_path: &str,
) -> Result<PreparedTargetWrite, MigrationError> {
    let target_cleanup = strip_retired_codegraph(target);
    let additions_cleanup = strip_retired_codegraph(additions);
    let merged = merge_without_clobber(&target_cleanup.document, &additions_cleanup.document);
    let marker = marker_value(target, migration_id, target_path)?;
    let mut document = merged.merged;
    if let Value::Object(ref mut map) = document {
        map.insert("_migrations".to_string(), json!(marker));
    }
    validate_target(target_path, &document)?;
    // additionsCleanup strips codegraph from the migration transform output before merging.
    // Its edits are intentionally omitted here: migration transforms are source-controlled and
    // should never emit codegraph; even if they did, the merge would exclude it from the
    // resulting document, so no explicit delete edit is needed for the additions side.
    let mut edits = target_cleanup.edits.clone();
    edits.extend(collect_migration_edits(&merged.additions, &[]));
    edits.push(OmoConfigEdit {
        path: vec![PathSegment::Key("_migrations".to_string())],
        value: Some(json!(marker)),
    });
    Ok(PreparedTargetWrite {
        diagnostics: unique_diagnostics(
            &[
                merged.diagnostics.clone(),
                target_cleanup.diagnostics.clone(),
                additions_cleanup.diagnostics.clone(),
            ]
            .concat(),
        ),
        document,
        edits,
    })
}

pub fn prepare_target_replacement(
    document: &Value,
    migration_id: &str,
    target: &Value,
    target_path: &str,
) -> Result<PreparedTargetWrite, MigrationError> {
    let target_cleanup = strip_retired_codegraph(target);
    let document_cleanup = strip_retired_codegraph(document);
    let marker = marker_value(target, migration_id, target_path)?;
    let mut full_doc = match document_cleanup.document.clone() {
        Value::Object(map) => Value::Object(map),
        _ => Value::Object(Map::new()),
    };
    if let Value::Object(ref mut map) = full_doc {
        map.insert("_migrations".to_string(), json!(marker));
    }
    validate_target(target_path, &full_doc)?;
    let mut edits = target_cleanup.edits.clone();

    if let Some(t_map) = target_cleanup.document.as_object() {
        for key in t_map.keys() {
            if key != "_migrations" && document_cleanup.document.get(key).is_none() {
                edits.push(OmoConfigEdit {
                    path: vec![PathSegment::Key(key.clone())],
                    value: None,
                });
            }
        }
    }
    if let Some(d_map) = document_cleanup.document.as_object() {
        for (key, value) in d_map {
            if key != "_migrations" {
                edits.push(OmoConfigEdit {
                    path: vec![PathSegment::Key(key.clone())],
                    value: Some(value.clone()),
                });
            }
        }
    }
    edits.push(OmoConfigEdit {
        path: vec![PathSegment::Key("_migrations".to_string())],
        value: Some(json!(marker)),
    });
    Ok(PreparedTargetWrite {
        diagnostics: unique_diagnostics(
            &[
                target_cleanup.diagnostics.clone(),
                document_cleanup.diagnostics.clone(),
            ]
            .concat(),
        ),
        document: full_doc,
        edits,
    })
}

pub fn write_prepared_target<'a>(
    env: &'a MigrationEnvironment,
    file_system: &'a dyn MigrationFileSystem,
    prepared: &'a PreparedTargetWrite,
    target_path: &'a str,
    write_target: &MigrationTargetWriter<'a>,
) -> Result<(), MigrationError> {
    write_target(MigrationTargetWriterInput {
        edits: &prepared.edits,
        env,
        file_system,
        target_path,
    })
}
