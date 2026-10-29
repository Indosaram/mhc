use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::atomic_write::write_file_atomically;
use crate::logger::log;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationsSidecar {
    pub applied_migrations: Vec<String>,
}

pub fn get_sidecar_path(config_path: impl AsRef<Path>) -> PathBuf {
    PathBuf::from(format!(
        "{}.migrations.json",
        config_path.as_ref().display()
    ))
}

/// Applied migration keys from `<config>.migrations.json`; empty on any read or shape failure.
pub fn read_applied_migrations(config_path: impl AsRef<Path>) -> HashSet<String> {
    let sidecar_path = get_sidecar_path(config_path);
    if !sidecar_path.exists() {
        return HashSet::new();
    }
    let parsed = fs::read_to_string(&sidecar_path)
        .map_err(|error| error.to_string())
        .and_then(|content| {
            serde_json::from_str::<Value>(&content).map_err(|error| error.to_string())
        });
    match parsed {
        Ok(value) => value
            .get("appliedMigrations")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        Err(error) => {
            log(
                &format!(
                    "[migration] Failed to read migrations sidecar at {}",
                    sidecar_path.display()
                ),
                Some(&json!(error)),
            );
            HashSet::new()
        }
    }
}

/// Persist sorted migration keys atomically; `false` when the write fails.
pub fn write_applied_migrations(
    config_path: impl AsRef<Path>,
    migrations: &HashSet<String>,
) -> bool {
    let sidecar_path = get_sidecar_path(config_path);
    let body = MigrationsSidecar {
        applied_migrations: migrations
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
    };
    let written = sidecar_path
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| serde_json::to_string_pretty(&body).map_err(std::io::Error::other))
        .and_then(|content| write_file_atomically(&sidecar_path, &format!("{content}\n")));
    match written {
        Ok(()) => true,
        Err(error) => {
            log(
                &format!(
                    "[migration] Failed to write migrations sidecar at {}",
                    sidecar_path.display()
                ),
                Some(&json!(error.to_string())),
            );
            false
        }
    }
}
