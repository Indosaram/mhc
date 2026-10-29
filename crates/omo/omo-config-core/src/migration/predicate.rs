use serde_json::Value;

pub fn has_migration_marker(target: &Value, migration_id: &str) -> bool {
    target
        .get("_migrations")
        .and_then(Value::as_array)
        .map(|markers| {
            markers
                .iter()
                .any(|marker| marker.as_str() == Some(migration_id))
        })
        .unwrap_or(false)
}

pub struct ShouldRunMigrationInput<'a> {
    pub legacy_sources_exist: bool,
    pub migration_id: &'a str,
    pub target: &'a Value,
}

pub fn should_run_migration(input: &ShouldRunMigrationInput<'_>) -> bool {
    input.legacy_sources_exist && !has_migration_marker(input.target, input.migration_id)
}
