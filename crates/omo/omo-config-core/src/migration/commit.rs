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

pub fn prepare_target_write(
    additions: &Value,
    migration_id: &str,
    target: &Value,
    target_path: &str,
) -> Result<PreparedTargetWrite, MigrationError> {
    let merged = merge_without_clobber(target, additions);
    let marker = marker_value(target, migration_id, target_path)?;
    let mut document = merged.merged;
    if let Value::Object(ref mut map) = document {
        map.insert("_migrations".to_string(), json!(marker));
    }
    validate_target(target_path, &document)?;
    let mut edits = collect_migration_edits(&merged.additions, &[]);
    edits.push(OmoConfigEdit {
        path: vec![PathSegment::Key("_migrations".to_string())],
        value: Some(json!(marker)),
    });
    Ok(PreparedTargetWrite {
        diagnostics: merged.diagnostics,
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
    let marker = marker_value(target, migration_id, target_path)?;
    let mut full_doc = document.clone();
    if let Value::Object(ref mut map) = full_doc {
        map.insert("_migrations".to_string(), json!(marker));
    }
    validate_target(target_path, &full_doc)?;
    let mut edits = Vec::new();
    let target_map = target.as_object();
    let doc_map = document.as_object();

    if let Some(t_map) = target_map {
        for key in t_map.keys() {
            if key != "_migrations" && (doc_map.is_none() || !doc_map.unwrap().contains_key(key)) {
                edits.push(OmoConfigEdit {
                    path: vec![PathSegment::Key(key.clone())],
                    value: None,
                });
            }
        }
    }
    if let Some(d_map) = doc_map {
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
        diagnostics: Vec::new(),
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
