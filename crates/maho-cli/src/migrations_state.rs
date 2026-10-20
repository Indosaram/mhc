use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
pub const MIGRATIONS_STATE_SCHEMA_VERSION: u64 = 1;
pub const MIGRATIONS_STATE_FILENAME: &str = "migrations-state.json";
pub const SCAN_MIGRATIONS: [&str; 3] = [
    "migrateLegacySenpiDirs",
    "migrateSessionsFromAgentRoot",
    "restoreDrainedPiDirs",
];
pub fn read_completed_scan_migrations(agent_dir: &Path) -> BTreeSet<String> {
    let Ok(raw) = fs::read_to_string(agent_dir.join(MIGRATIONS_STATE_FILENAME)) else {
        return BTreeSet::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return BTreeSet::new();
    };
    if value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        != Some(MIGRATIONS_STATE_SCHEMA_VERSION)
    {
        return BTreeSet::new();
    }
    let Some(completed) = value.get("completed").and_then(serde_json::Value::as_array) else {
        return BTreeSet::new();
    };
    if completed.iter().any(|v| !v.is_string()) {
        return BTreeSet::new();
    }
    completed
        .iter()
        .filter_map(serde_json::Value::as_str)
        .filter(|s| SCAN_MIGRATIONS.contains(s))
        .map(str::to_owned)
        .collect()
}
pub fn write_completed_scan_migrations(
    completed: &[&str],
    agent_dir: &Path,
) -> std::io::Result<()> {
    let mut state = read_state(agent_dir);
    state.insert("completed".to_owned(), serde_json::json!(completed));
    write_state(agent_dir, state)
}

#[derive(Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyPiAgentDirRecord {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copied_at: Option<f64>,
    pub noticed_mtimes: std::collections::BTreeMap<String, f64>,
}

fn read_state(agent_dir: &Path) -> serde_json::Map<String, serde_json::Value> {
    let parsed = fs::read(agent_dir.join(MIGRATIONS_STATE_FILENAME))
        .ok()
        .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok());
    match parsed {
        Some(serde_json::Value::Object(state))
            if state
                .get("schemaVersion")
                .and_then(serde_json::Value::as_u64)
                == Some(MIGRATIONS_STATE_SCHEMA_VERSION) =>
        {
            state
        }
        _ => serde_json::Map::new(),
    }
}

pub fn read_legacy_pi_agent_dir_record(agent_dir: &Path) -> LegacyPiAgentDirRecord {
    let state = read_state(agent_dir);
    let raw = state.get("legacyPiAgentDir");
    let copied_at = raw
        .and_then(|raw| raw.get("copiedAt"))
        .and_then(serde_json::Value::as_f64)
        .filter(|n| n.is_finite());
    let noticed_mtimes = raw
        .and_then(|raw| raw.get("noticedMtimes"))
        .and_then(serde_json::Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(name, value)| {
                    value
                        .as_f64()
                        .filter(|n| n.is_finite())
                        .map(|n| (name.clone(), n))
                })
                .collect()
        })
        .unwrap_or_default();
    LegacyPiAgentDirRecord {
        copied_at,
        noticed_mtimes,
    }
}

pub fn write_legacy_pi_agent_dir_record(
    record: &LegacyPiAgentDirRecord,
    agent_dir: &Path,
) -> std::io::Result<()> {
    let mut state = read_state(agent_dir);
    let completed = read_completed_scan_migrations(agent_dir);
    state.insert("completed".to_owned(), serde_json::json!(completed));
    state.insert("legacyPiAgentDir".to_owned(), serde_json::to_value(record)?);
    write_state(agent_dir, state)
}

pub fn record_legacy_pi_agent_dir_copy(agent_dir: &Path) -> std::io::Result<()> {
    let copied_at = crate::legacy_pi_edits::mtime_ms(std::time::SystemTime::now());
    write_legacy_pi_agent_dir_record(
        &LegacyPiAgentDirRecord {
            copied_at: Some(copied_at),
            ..Default::default()
        },
        agent_dir,
    )
}

fn write_state(
    agent_dir: &Path,
    mut state: serde_json::Map<String, serde_json::Value>,
) -> std::io::Result<()> {
    state.insert(
        "schemaVersion".to_owned(),
        serde_json::json!(MIGRATIONS_STATE_SCHEMA_VERSION),
    );
    let target = agent_dir.join(MIGRATIONS_STATE_FILENAME);
    let temporary = agent_dir.join(format!(
        "{MIGRATIONS_STATE_FILENAME}.{}.tmp",
        std::process::id()
    ));
    let result: std::io::Result<()> = (|| {
        fs::create_dir_all(agent_dir)?;
        fs::write(
            &temporary,
            format!("{}\n", serde_json::to_string_pretty(&state)?),
        )?;
        fs::rename(&temporary, &target)
    })();
    if let Err(error) = result {
        eprintln!(
            "Could not save migration state {}: {error}",
            target.display()
        );
        if let Err(cleanup) = fs::remove_file(&temporary) {
            if cleanup.kind() != std::io::ErrorKind::NotFound {
                eprintln!("Could not remove {}: {cleanup}", temporary.display());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_write_preserves_copy_history_and_unknown_fields() {
        // Given
        let root = tempfile::tempdir().expect("tempdir");
        fs::write(root.path().join(MIGRATIONS_STATE_FILENAME), r#"{"schemaVersion":1,"custom":{"keep":true},"legacyPiAgentDir":{"copiedAt":123,"noticedMtimes":{"settings.json":456}}}"#).expect("state");
        // When
        write_completed_scan_migrations(&SCAN_MIGRATIONS, root.path()).expect("write");
        // Then
        let state = read_state(root.path());
        assert_eq!(state["custom"], serde_json::json!({"keep":true}));
        assert_eq!(
            state["legacyPiAgentDir"]["noticedMtimes"]["settings.json"],
            456
        );
        assert_eq!(read_completed_scan_migrations(root.path()).len(), 3);
    }

    #[test]
    fn failed_state_rename_is_nonfatal_and_cleans_temporary_file() {
        // Given
        let root = tempfile::tempdir().expect("tempdir");
        fs::create_dir(root.path().join(MIGRATIONS_STATE_FILENAME)).expect("block rename");
        // When
        let result = write_completed_scan_migrations(&SCAN_MIGRATIONS, root.path());
        // Then
        assert!(result.is_ok());
        assert!(read_completed_scan_migrations(root.path()).is_empty());
        assert_eq!(fs::read_dir(root.path()).expect("entries").count(), 1);
    }
}
