use serde_json::{Map, Value, json};

use crate::internal::jsonc::parse_jsonc_safe;
use crate::internal::plain_object::is_plain_object;
use crate::internal::posix_path::{posix_join, to_posix_path};
use crate::loader::paths::resolve_home_dir;
pub use crate::migration::types::MigrationBackupMove;
use crate::migration::types::{
    MigrationClock, MigrationEnvironment, MigrationError, MigrationFileSystem,
};
use crate::writer::types::FsErrorKind;

#[derive(Debug, Clone, PartialEq)]
pub struct MigrationJournalTargetWrite {
    pub additions: Value,
    pub mode: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MigrationJournal {
    pub backup_moves: Vec<MigrationBackupMove>,
    pub completed_moves: Vec<String>,
    pub diagnostics: Vec<String>,
    pub migration_id: String,
    pub target_path: String,
    pub target_write: MigrationJournalTargetWrite,
    pub target_written: bool,
    pub version: u32,
}

pub fn migration_journal_path(env: &MigrationEnvironment) -> String {
    to_posix_path(&posix_join(&[
        &resolve_home_dir(env),
        ".maho",
        ".migration-journal.json",
    ]))
}

fn journal_temp_path(path: &str, pid: u32, now: i64, attempt: usize) -> String {
    let suffix = format!("{pid}.{now}");
    if attempt == 0 {
        format!("{path}.{suffix}.tmp")
    } else {
        format!("{path}.{suffix}.{attempt}.tmp")
    }
}

pub fn parse_journal(value: &Value) -> Result<MigrationJournal, MigrationError> {
    if !is_plain_object(value) {
        return Err(MigrationError::transaction(
            "Migration journal must be an object",
        ));
    }
    let map = value.as_object().unwrap();

    if map.get("version").and_then(Value::as_i64) != Some(1) {
        return Err(MigrationError::transaction(
            "Migration journal version is unsupported",
        ));
    }

    let target_path = match map.get("targetPath").and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => {
            return Err(MigrationError::transaction(
                "Migration journal target is invalid",
            ));
        }
    };
    let migration_id = match map.get("migrationId").and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => {
            return Err(MigrationError::transaction(
                "Migration journal target is invalid",
            ));
        }
    };

    let target_write_val = match map.get("targetWrite") {
        Some(v) if is_plain_object(v) => v,
        _ => {
            return Err(MigrationError::transaction(
                "Migration journal target write is invalid",
            ));
        }
    };
    let target_write_obj = target_write_val.as_object().unwrap();

    let additions = match target_write_obj.get("additions") {
        Some(v) if is_plain_object(v) => v.clone(),
        _ => {
            return Err(MigrationError::transaction(
                "Migration journal target write is invalid",
            ));
        }
    };

    let target_write_mode = match target_write_obj.get("mode") {
        Some(Value::String(s)) => {
            if s == "replace-target" {
                Some("replace-target".to_string())
            } else {
                return Err(MigrationError::transaction(
                    "Migration journal target write mode is invalid",
                ));
            }
        }
        Some(_) => {
            return Err(MigrationError::transaction(
                "Migration journal target write mode is invalid",
            ));
        }
        None => None,
    };

    let target_written = match map.get("targetWritten").and_then(Value::as_bool) {
        Some(b) => b,
        None => {
            return Err(MigrationError::transaction(
                "Migration journal completion state is invalid",
            ));
        }
    };

    let completed_moves_val = match map.get("completedMoves").and_then(Value::as_array) {
        Some(arr) => arr,
        None => {
            return Err(MigrationError::transaction(
                "Migration journal completion state is invalid",
            ));
        }
    };
    let mut completed_moves = Vec::new();
    for item in completed_moves_val {
        match item.as_str() {
            Some(s) => completed_moves.push(s.to_string()),
            None => {
                return Err(MigrationError::transaction(
                    "Migration journal completed moves are invalid",
                ));
            }
        }
    }

    let mut diagnostics = Vec::new();
    if let Some(diag_val) = map.get("diagnostics") {
        let diag_arr = match diag_val.as_array() {
            Some(arr) => arr,
            None => {
                return Err(MigrationError::transaction(
                    "Migration journal diagnostics are invalid",
                ));
            }
        };
        for item in diag_arr {
            match item.as_str() {
                Some(s) => diagnostics.push(s.to_string()),
                None => {
                    return Err(MigrationError::transaction(
                        "Migration journal diagnostics are invalid",
                    ));
                }
            }
        }
    }

    let backup_moves_val = match map.get("backupMoves").and_then(Value::as_array) {
        Some(arr) => arr,
        None => {
            return Err(MigrationError::transaction(
                "Migration journal backup plan is invalid",
            ));
        }
    };
    let mut backup_moves = Vec::new();
    for item in backup_moves_val {
        if !is_plain_object(item) {
            return Err(MigrationError::transaction(
                "Migration journal backup move is invalid",
            ));
        }
        let move_obj = item.as_object().unwrap();
        let from = match move_obj.get("from").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None => {
                return Err(MigrationError::transaction(
                    "Migration journal backup move is invalid",
                ));
            }
        };
        let to = match move_obj.get("to").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None => {
                return Err(MigrationError::transaction(
                    "Migration journal backup move is invalid",
                ));
            }
        };
        backup_moves.push(MigrationBackupMove { from, to });
    }

    Ok(MigrationJournal {
        backup_moves,
        completed_moves,
        diagnostics,
        migration_id,
        target_path,
        target_write: MigrationJournalTargetWrite {
            additions,
            mode: target_write_mode,
        },
        target_written,
        version: 1,
    })
}

pub fn read_migration_journal(
    file_system: &dyn MigrationFileSystem,
    env: &MigrationEnvironment,
) -> Result<Option<MigrationJournal>, MigrationError> {
    let path = migration_journal_path(env);
    if !file_system.exists(&path) {
        return Ok(None);
    }
    let content = file_system.read(&path)?;
    let parsed = parse_jsonc_safe(&content);
    if !parsed.errors.is_empty() || parsed.data.is_none() {
        return Err(MigrationError::transaction(
            "Migration journal must be an object",
        ));
    }
    parse_journal(&parsed.data.unwrap()).map(Some)
}

pub fn write_migration_journal(
    journal: &MigrationJournal,
    file_system: &dyn MigrationFileSystem,
    env: &MigrationEnvironment,
    pid: u32,
    clock: &dyn MigrationClock,
) -> Result<(), MigrationError> {
    let path = migration_journal_path(env);
    let mut target_write_map = Map::new();
    target_write_map.insert(
        "additions".to_string(),
        journal.target_write.additions.clone(),
    );
    if let Some(mode) = &journal.target_write.mode {
        target_write_map.insert("mode".to_string(), json!(mode));
    }

    let backup_moves_json = journal
        .backup_moves
        .iter()
        .map(|m| json!({ "from": m.from, "to": m.to }))
        .collect::<Vec<_>>();

    let mut journal_map = Map::new();
    journal_map.insert("backupMoves".to_string(), json!(backup_moves_json));
    journal_map.insert("completedMoves".to_string(), json!(journal.completed_moves));
    journal_map.insert("diagnostics".to_string(), json!(journal.diagnostics));
    journal_map.insert("migrationId".to_string(), json!(journal.migration_id));
    journal_map.insert("targetPath".to_string(), json!(journal.target_path));
    journal_map.insert("targetWrite".to_string(), Value::Object(target_write_map));
    journal_map.insert("targetWritten".to_string(), json!(journal.target_written));
    journal_map.insert("version".to_string(), json!(1));

    let content = format!("{}\n", Value::Object(journal_map));
    let now = clock.now();
    let mut attempt = 0usize;
    loop {
        let temp_path = journal_temp_path(&path, pid, now, attempt);
        match file_system.write_exclusive(&temp_path, &content) {
            Ok(()) => {
                file_system.rename(&temp_path, &path)?;
                return Ok(());
            }
            Err(error) => {
                if error.kind != FsErrorKind::Exists {
                    return Err(MigrationError::Fs(error.message));
                }
                attempt += 1;
            }
        }
    }
}

pub fn remove_migration_journal(
    file_system: &dyn MigrationFileSystem,
    env: &MigrationEnvironment,
) -> Result<(), MigrationError> {
    let path = migration_journal_path(env);
    if file_system.exists(&path) {
        file_system.unlink(&path)?;
    }
    Ok(())
}
