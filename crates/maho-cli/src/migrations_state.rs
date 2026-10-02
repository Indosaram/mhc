use std::collections::BTreeSet;
use std::path::Path;
use std::fs;
pub const MIGRATIONS_STATE_SCHEMA_VERSION: u64 = 1;
pub const MIGRATIONS_STATE_FILENAME: &str = "migrations-state.json";
pub const SCAN_MIGRATIONS: [&str; 2] = ["migrateLegacySenpiDirs", "migrateSessionsFromAgentRoot"];
pub fn read_completed_scan_migrations(agent_dir: &Path) -> BTreeSet<String> {
    let Ok(raw) = fs::read_to_string(agent_dir.join(MIGRATIONS_STATE_FILENAME)) else { return BTreeSet::new(); };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else { return BTreeSet::new(); };
    if value.get("schemaVersion").and_then(serde_json::Value::as_u64) != Some(MIGRATIONS_STATE_SCHEMA_VERSION) { return BTreeSet::new(); }
    let Some(completed) = value.get("completed").and_then(serde_json::Value::as_array) else { return BTreeSet::new(); };
    if completed.iter().any(|v| !v.is_string()) { return BTreeSet::new(); }
    completed.iter().filter_map(serde_json::Value::as_str).filter(|s| SCAN_MIGRATIONS.contains(s)).map(str::to_owned).collect()
}
pub fn write_completed_scan_migrations(completed: &[&str], agent_dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(agent_dir)?;
    let target = agent_dir.join(MIGRATIONS_STATE_FILENAME);
    let temporary = agent_dir.join(format!("{MIGRATIONS_STATE_FILENAME}.{}.tmp", std::process::id()));
    let result = (|| { fs::write(&temporary, format!("{}\n", serde_json::to_string_pretty(&serde_json::json!({"schemaVersion": MIGRATIONS_STATE_SCHEMA_VERSION, "completed": completed}))?))?; fs::rename(&temporary, &target) })();
    if result.is_err() && temporary.exists() { fs::remove_file(temporary)?; }
    result
}
