use crate::internal::jsonc::parse_jsonc_safe;
use crate::internal::jsonc::{FormattingOptions, ModificationOptions};
use crate::writer::surgical_edit::apply_omo_config_edit;
use crate::internal::posix_path::{posix_dirname, posix_join};
use crate::loader::paths::{process_env, resolve_user_omo_config_path};
use crate::loader::types::OmoConfigEnv;
use crate::writer::types::{
    FsErrorKind, OmoConfigWriteError, OmoConfigWriteFileSystem, StdWriteFileSystem,
    UpdateOmoConfigOptions, UpdateOmoConfigResult,
};

const EMPTY_OMO_CONFIG: &str = "// OMO configuration\n{\n}\n";

pub fn formatting_options() -> ModificationOptions {
    ModificationOptions::formatted(FormattingOptions {
        eol: "\n".to_string(),
        insert_spaces: true,
        tab_size: 2,
        keep_lines: false,
        insert_final_newline: false,
    })
}

pub fn backup_suffix(timestamp: &str) -> String {
    timestamp.replace([':', '.'], "-")
}

fn default_timestamp() -> String {
    jiff::Timestamp::now().to_string()
}

fn backup_candidate(base_path: &str, attempt: usize) -> String {
    if attempt == 0 {
        base_path.to_string()
    } else {
        format!("{base_path}.{attempt}")
    }
}

fn write_backup(
    path: &str,
    content: &str,
    file_system: &dyn OmoConfigWriteFileSystem,
    timestamp: &str,
) -> Result<String, OmoConfigWriteError> {
    let base_path = format!("{path}.bak.{timestamp}");
    let mut attempt = 0usize;
    loop {
        let candidate = backup_candidate(&base_path, attempt);
        match file_system.write_exclusive(&candidate, content) {
            Ok(()) => return Ok(candidate),
            Err(error) => {
                if error.kind != FsErrorKind::Exists {
                    return Err(OmoConfigWriteError::new(path, "backup", error.message));
                }
                attempt += 1;
            }
        }
    }
}

fn write_planned_backup(
    planned_path: &str,
    content: &str,
    file_system: &dyn OmoConfigWriteFileSystem,
) -> Result<String, OmoConfigWriteError> {
    if file_system.exists(planned_path)
        && file_system.read(planned_path).is_ok_and(|existing| existing == content)
    {
        return Ok(planned_path.to_string());
    }
    let mut attempt = 0usize;
    loop {
        let candidate = backup_candidate(planned_path, attempt);
        match file_system.write_exclusive(&candidate, content) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind == FsErrorKind::Exists => attempt += 1,
            Err(error) => return Err(OmoConfigWriteError::new(planned_path, "backup", error.message)),
        }
    }
}

fn resolve_write_path(
    options: &UpdateOmoConfigOptions<'_>,
    file_system: &dyn OmoConfigWriteFileSystem,
    env: &OmoConfigEnv,
) -> String {
    if let Some(target_path) = &options.target_path {
        return target_path.clone();
    }
    if options.scope == "user" {
        let jsonc_path = resolve_user_omo_config_path(env);
        if file_system.exists(&jsonc_path) {
            return jsonc_path;
        }
        let json_path = posix_join(&[posix_dirname(&jsonc_path).as_str(), "omo.json"]);
        return if file_system.exists(&json_path) {
            json_path
        } else {
            jsonc_path
        };
    }
    let project_dir = options.project_dir.clone().unwrap_or_else(|| {
        std::env::current_dir()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_default()
    });
    let jsonc_path = posix_join(&[project_dir.as_str(), ".omo", "omo.jsonc"]);
    if file_system.exists(&jsonc_path) {
        return jsonc_path;
    }
    let json_path = posix_join(&[posix_dirname(&jsonc_path).as_str(), "omo.json"]);
    if file_system.exists(&json_path) {
        json_path
    } else {
        jsonc_path
    }
}

fn write_atomically(
    path: &str,
    content: &str,
    file_system: &dyn OmoConfigWriteFileSystem,
) -> Result<(), OmoConfigWriteError> {
    let temp_path = format!("{path}.{}.tmp", uuid::Uuid::new_v4());
    let mut temp_created = false;
    let outcome = (|| -> Result<(), String> {
        file_system
            .write_exclusive(&temp_path, content)
            .map_err(|error| error.message)?;
        temp_created = true;
        file_system
            .rename(&temp_path, path)
            .map_err(|error| error.message)?;
        Ok(())
    })();
    if let Err(detail) = outcome {
        if temp_created {
            let _ = file_system.unlink(&temp_path);
        }
        return Err(OmoConfigWriteError::new(path, "write", detail));
    }
    Ok(())
}

fn assert_config_path_is_safe(
    path: &str,
    file_system: &dyn OmoConfigWriteFileSystem,
) -> Result<(), OmoConfigWriteError> {
    match file_system.is_symbolic_link(path) {
        Ok(true) => Err(OmoConfigWriteError::new(
            path,
            "read",
            "Refusing to edit symlinked omo config",
        )),
        Ok(false) => Ok(()),
        Err(error) => Err(OmoConfigWriteError::new(path, "read", error.message)),
    }
}

fn assert_project_config_directory_is_safe(
    directory: &str,
    file_system: &dyn OmoConfigWriteFileSystem,
) -> Result<(), OmoConfigWriteError> {
    match file_system.is_symbolic_link(directory) {
        Ok(true) => Err(OmoConfigWriteError::new(
            directory,
            "read",
            "Refusing to edit config under symlinked project .omo directory",
        )),
        Ok(false) => Ok(()),
        Err(error) => Err(OmoConfigWriteError::new(directory, "read", error.message)),
    }
}

fn assert_jsonc_can_be_modified(path: &str, content: &str) -> Result<(), OmoConfigWriteError> {
    let parsed = parse_jsonc_safe(content);
    if parsed.errors.is_empty() {
        return Ok(());
    }
    let message = parsed
        .errors
        .iter()
        .map(|error| format!("{} at offset {}", error.message, error.offset))
        .collect::<Vec<_>>()
        .join(", ");
    Err(OmoConfigWriteError::new(path, "parse", message))
}

pub fn update_omo_config(
    options: &UpdateOmoConfigOptions<'_>,
) -> Result<UpdateOmoConfigResult, OmoConfigWriteError> {
    let std_file_system = StdWriteFileSystem;
    let file_system: &dyn OmoConfigWriteFileSystem =
        options.file_system.unwrap_or(&std_file_system);
    let default_env = process_env();
    let env = options.env.clone().unwrap_or(default_env);
    let path = resolve_write_path(options, file_system, &env);
    let directory = posix_dirname(&path);
    let existed = file_system.exists(&path);
    let mut content = EMPTY_OMO_CONFIG.to_string();

    let read_outcome = (|| -> Result<(), OmoConfigWriteError> {
        file_system
            .mkdirs(&directory)
            .map_err(|error| OmoConfigWriteError::new(&path, "read", error.message))?;
        if options.scope == "project" {
            assert_project_config_directory_is_safe(&directory, file_system)?;
        }
        if existed {
            assert_config_path_is_safe(&path, file_system)?;
            content = file_system
                .read(&path)
                .map_err(|error| OmoConfigWriteError::new(&path, "read", error.message))?;
        }
        Ok(())
    })();
    read_outcome?;

    assert_jsonc_can_be_modified(&path, &content)?;

    let mut backup_path: Option<String> = None;
    if existed {
        assert_config_path_is_safe(&path, file_system)?;
        let timestamp = backup_suffix(&options.timestamp.clone().unwrap_or_else(default_timestamp));
        backup_path = Some(match options.backup_path.as_deref() {
            Some(planned_path) => write_planned_backup(planned_path, &content, file_system)?,
            None => write_backup(&path, &content, file_system, &timestamp)?,
        });
    }

    let mut next_content = content;
    for edit in &options.edits {
        next_content = apply_omo_config_edit(&next_content, edit)
            .map_err(|error| {
                let operation = if error == crate::internal::jsonc::JsoncEditError::OverlappingEdit {
                    "write"
                } else {
                    "parse"
                };
                OmoConfigWriteError::new(&path, operation, error)
            })?;
    }

    write_atomically(&path, &next_content, file_system)?;
    Ok(UpdateOmoConfigResult { backup_path, path })
}

#[cfg(test)]
mod parity_5138_tests {
    use super::*;
    use crate::issue::PathSegment;
    use crate::writer::types::OmoConfigEdit;

    #[test]
    fn writer_reports_every_recovered_parse_error_before_touching_the_file() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("omo.jsonc");
        let content = "{\"a\":,\"b\":}";
        std::fs::write(&path, content).expect("fixture");
        let error = update_omo_config(&UpdateOmoConfigOptions {
            target_path: Some(path.to_string_lossy().to_string()),
            timestamp: Some("fixed".to_string()),
            edits: vec![OmoConfigEdit::set(vec![PathSegment::Key("c".to_string())], serde_json::json!(3))],
            ..UpdateOmoConfigOptions::default()
        }).expect_err("invalid jsonc must not be edited");
        assert_eq!(error.operation, "parse");
        assert_eq!(error.detail, "ValueExpected at offset 5, ValueExpected at offset 10");
        assert_eq!(std::fs::read_to_string(&path).expect("unchanged"), content);
    }

    #[test]
    fn writer_routes_insertions_and_removals_through_surgical_edit() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("omo.jsonc");
        let content = "{\r\n\t\"a\": 1, // keep\r\n\t\"b\": 2\r\n}\r\n";
        std::fs::write(&path, content).expect("fixture");
        let result = update_omo_config(&UpdateOmoConfigOptions {
            target_path: Some(path.to_string_lossy().to_string()),
            timestamp: Some("fixed".to_string()),
            edits: vec![
                OmoConfigEdit::set(vec![PathSegment::Key("c".to_string())], serde_json::json!(3)),
                OmoConfigEdit::remove(vec![PathSegment::Key("b".to_string())]),
            ],
            ..UpdateOmoConfigOptions::default()
        }).expect("write");
        assert_eq!(std::fs::read_to_string(&path).expect("result"), "{\r\n\t\"a\": 1, // keep\r\n\t\"c\": 3\r\n}\r\n");
        assert_eq!(std::fs::read_to_string(result.backup_path.expect("backup path")).expect("backup"), content);
    }
}
