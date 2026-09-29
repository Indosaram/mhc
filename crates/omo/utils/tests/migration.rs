//! Translated from migration/agent-names, migration/config-migration and migration/migrations-sidecar tests.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};
use tempfile::TempDir;
use utils::*;

const MIGRATION_KEY: &str = "model-version:anthropic/claude-opus-4-4->anthropic/claude-opus-4-8";

fn obj(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("expected object, got {other}"),
    }
}

fn alias(name: &str) -> Option<&'static str> {
    AGENT_NAME_MAP
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| *value)
}

#[test]
fn agent_name_map_parenthesized_aliases() {
    let cases = [
        ("Sisyphus (Ultraworker)", "sisyphus"),
        ("Hephaestus (Deep Agent)", "hephaestus"),
        ("Prometheus (Plan Builder)", "prometheus"),
        ("Atlas (Plan Executor)", "atlas"),
        ("Metis (Plan Consultant)", "metis"),
        ("Momus (Plan Critic)", "momus"),
    ];
    for (name, expected) in cases {
        assert_eq!(alias(name), Some(expected), "{name}");
    }
}

#[test]
fn migrate_agent_names_rewrites_parenthesized_aliases() {
    let legacy = obj(json!({
        "Sisyphus (Ultraworker)": {"model": "claude-opus-4"},
        "Hephaestus (Deep Agent)": {"model": "gpt-5.4"},
        "Prometheus (Plan Builder)": {"model": "claude-opus-4"},
        "Atlas (Plan Executor)": {"model": "kimi-k2.5"},
        "Metis (Plan Consultant)": {"model": "claude-opus-4"},
        "Momus (Plan Critic)": {"model": "claude-opus-4"},
    }));
    let result = migrate_agent_names(&legacy);
    assert!(result.changed);
    assert_eq!(
        Value::Object(result.migrated),
        json!({
            "sisyphus": {"model": "claude-opus-4"},
            "hephaestus": {"model": "gpt-5.4"},
            "prometheus": {"model": "claude-opus-4"},
            "atlas": {"model": "kimi-k2.5"},
            "metis": {"model": "claude-opus-4"},
            "momus": {"model": "claude-opus-4"},
        })
    );
}

fn legacy_config() -> Map<String, Value> {
    obj(json!({"agents": {"prometheus": {"model": "anthropic/claude-opus-4-4"}}}))
}

fn write_json(path: &Path, value: &Map<String, Value>) {
    let body = serde_json::to_string_pretty(value).unwrap_or_default();
    fs::write(path, format!("{body}\n")).unwrap_or_else(|error| panic!("{error}"));
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap_or_default()).unwrap_or(Value::Null)
}

fn backup_count(dir: &Path) -> usize {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().contains(".bak."))
                .count()
        })
        .unwrap_or_default()
}

fn tempdir() -> TempDir {
    TempDir::new().unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn migrate_config_writes_config_before_sidecar() {
    let dir = tempdir();
    let config_path = dir.path().join("oh-my-opencode.json");
    let mut raw = legacy_config();
    write_json(&config_path, &raw);

    assert!(migrate_config_file(&config_path, &mut raw));

    assert_eq!(
        Value::Object(raw),
        json!({"agents": {"prometheus": {"model": "anthropic/claude-opus-4-8"}}})
    );
    assert_eq!(
        read_json(&config_path),
        json!({"agents": {"prometheus": {"model": "anthropic/claude-opus-4-8"}}})
    );
    assert_eq!(
        read_json(&get_sidecar_path(&config_path)),
        json!({"appliedMigrations": [MIGRATION_KEY]})
    );
}

#[test]
fn migrate_config_skips_sidecar_when_config_write_fails_and_retries_later() {
    let dir = tempdir();
    let config_path = dir
        .path()
        .join("missing-parent")
        .join("oh-my-opencode.json");
    let mut first = legacy_config();

    assert!(migrate_config_file(&config_path, &mut first));
    assert!(!get_sidecar_path(&config_path).exists());
    assert_eq!(first.get("_migrations"), Some(&json!([MIGRATION_KEY])));

    fs::create_dir_all(dir.path().join("missing-parent")).unwrap_or_else(|error| panic!("{error}"));
    write_json(&config_path, &legacy_config());
    let mut retried = legacy_config();

    assert!(migrate_config_file(&config_path, &mut retried));
    assert_eq!(
        Value::Object(retried),
        json!({"agents": {"prometheus": {"model": "anthropic/claude-opus-4-8"}}})
    );
    assert!(get_sidecar_path(&config_path).exists());
}

#[test]
fn migrate_config_preserves_migrations_field_when_sidecar_write_fails() {
    let dir = tempdir();
    let config_path = dir.path().join("oh-my-opencode.json");
    let mut raw = legacy_config();
    write_json(&config_path, &raw);
    fs::create_dir(get_sidecar_path(&config_path)).unwrap_or_else(|error| panic!("{error}"));

    assert!(migrate_config_file(&config_path, &mut raw));

    let expected = json!({"agents": {"prometheus": {"model": "anthropic/claude-opus-4-8"}}, "_migrations": [MIGRATION_KEY]});
    assert_eq!(Value::Object(raw), expected);
    assert_eq!(read_json(&config_path), expected);
    assert!(get_sidecar_path(&config_path).is_dir());
}

#[test]
fn migrate_config_treats_applied_migrations_as_history() {
    let dir = tempdir();
    let config_path = dir.path().join("oh-my-openagent.json");
    let history = "model-version:anthropic/claude-opus-4-6->anthropic/claude-opus-4-7";
    let mut raw = obj(json!({
        "agents": {"oracle": {"model": "anthropic/claude-opus-4-6"}},
        "appliedMigrations": [history],
    }));
    write_json(&config_path, &raw);

    assert!(migrate_config_file(&config_path, &mut raw));

    assert_eq!(
        Value::Object(raw),
        json!({"agents": {"oracle": {"model": "anthropic/claude-opus-4-6"}}})
    );
    assert_eq!(
        read_json(&get_sidecar_path(&config_path)),
        json!({"appliedMigrations": [history]})
    );
}

#[test]
fn migrate_config_skips_backup_when_content_is_identical() {
    let dir = tempdir();
    let config_path = dir.path().join("oh-my-opencode.json");
    write_json(
        &config_path,
        &obj(json!({"disabled_hooks": ["comment-checker"]})),
    );
    let mut raw =
        obj(json!({"disabled_hooks": ["gpt-permission-continuation", "comment-checker"]}));

    migrate_config_file(&config_path, &mut raw);

    assert_eq!(backup_count(dir.path()), 0);
}

#[test]
fn migrate_config_creates_backup_when_content_changes() {
    let dir = tempdir();
    let config_path = dir.path().join("oh-my-opencode.json");
    let mut raw = legacy_config();
    write_json(&config_path, &raw);

    assert!(migrate_config_file(&config_path, &mut raw));
    assert_eq!(backup_count(dir.path()), 1);
}

#[test]
fn migrate_config_removes_obsolete_lsp_key() {
    let dir = tempdir();
    let config_path = dir.path().join("oh-my-opencode.json");
    let mut raw = obj(json!({"lsp": {
        "typescript": {"command": ["typescript-language-server", "--stdio"]},
        "rust": {"command": ["rust-analyzer"]},
    }}));
    write_json(&config_path, &raw);

    assert!(migrate_config_file(&config_path, &mut raw));
    assert_eq!(raw.get("lsp"), None);
    assert_eq!(read_json(&config_path).get("lsp"), None);
}

#[test]
fn migrate_config_leaves_config_without_lsp_alone() {
    let dir = tempdir();
    let config_path = dir.path().join("oh-my-opencode.json");
    let mut raw = obj(json!({"agents": {"sisyphus": {"model": "anthropic/claude-opus-4-7"}}}));
    write_json(&config_path, &raw);

    assert!(!migrate_config_file(&config_path, &mut raw));
    assert_eq!(
        Value::Object(raw),
        json!({"agents": {"sisyphus": {"model": "anthropic/claude-opus-4-7"}}})
    );
}

#[test]
fn sidecar_path_appends_suffix() {
    assert_eq!(
        get_sidecar_path("/home/user/.config/opencode/oh-my-openagent.json"),
        Path::new("/home/user/.config/opencode/oh-my-openagent.json.migrations.json")
    );
    assert_eq!(
        get_sidecar_path("/home/user/oh-my-openagent.jsonc"),
        Path::new("/home/user/oh-my-openagent.jsonc.migrations.json")
    );
}

fn set(items: &[&str]) -> HashSet<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

#[test]
fn read_applied_migrations_empty_without_sidecar() {
    let dir = tempdir();
    assert_eq!(
        read_applied_migrations(dir.path().join("oh-my-openagent.json")),
        HashSet::new()
    );
}

#[test]
fn read_applied_migrations_reads_well_formed_sidecar() {
    let dir = tempdir();
    let config_path = dir.path().join("oh-my-openagent.json");
    let keys = [
        "model-version:openai/gpt-5.4->openai/gpt-5.5",
        "model-version:anthropic/claude-opus-4-5->anthropic/claude-opus-4-7",
    ];
    fs::write(
        get_sidecar_path(&config_path),
        json!({"appliedMigrations": keys}).to_string(),
    )
    .unwrap_or_default();
    assert_eq!(read_applied_migrations(&config_path), set(&keys));
}

#[test]
fn read_applied_migrations_tolerates_bad_payloads() {
    let dir = tempdir();
    let config_path = dir.path().join("oh-my-openagent.json");
    for body in [
        "{ this is not json".to_string(),
        json!({"appliedMigrations": "not-an-array"}).to_string(),
    ] {
        fs::write(get_sidecar_path(&config_path), body).unwrap_or_default();
        assert_eq!(read_applied_migrations(&config_path), HashSet::new());
    }
}

#[test]
fn read_applied_migrations_ignores_non_string_entries() {
    let dir = tempdir();
    let config_path = dir.path().join("oh-my-openagent.json");
    let body = json!({"appliedMigrations": ["model-version:a->b", 42, null, "model-version:c->d"]});
    fs::write(get_sidecar_path(&config_path), body.to_string()).unwrap_or_default();
    assert_eq!(
        read_applied_migrations(&config_path),
        set(&["model-version:a->b", "model-version:c->d"])
    );
}

#[test]
fn write_applied_migrations_creates_sidecar() {
    let dir = tempdir();
    let config_path = dir.path().join("oh-my-openagent.json");
    assert!(write_applied_migrations(
        &config_path,
        &set(&["model-version:openai/gpt-5.4->openai/gpt-5.5"])
    ));
    assert_eq!(
        read_json(&get_sidecar_path(&config_path)),
        json!({"appliedMigrations": ["model-version:openai/gpt-5.4->openai/gpt-5.5"]})
    );
}

#[test]
fn write_applied_migrations_sorts_entries() {
    let dir = tempdir();
    let config_path = dir.path().join("oh-my-openagent.json");
    write_applied_migrations(
        &config_path,
        &set(&[
            "model-version:z->y",
            "model-version:a->b",
            "model-version:m->n",
        ]),
    );
    assert_eq!(
        read_json(&get_sidecar_path(&config_path)),
        json!({"appliedMigrations": ["model-version:a->b", "model-version:m->n", "model-version:z->y"]})
    );
}

#[test]
fn write_applied_migrations_creates_parent_directories() {
    let dir = tempdir();
    let config_path = dir
        .path()
        .join("nested/dir/that/does/not/exist/oh-my-openagent.json");
    assert!(write_applied_migrations(
        &config_path,
        &set(&["model-version:a->b"])
    ));
    assert!(get_sidecar_path(&config_path).exists());
}

#[test]
fn applied_migrations_round_trip() {
    let dir = tempdir();
    let config_path = dir.path().join("oh-my-openagent.jsonc");
    let original = set(&[
        "model-version:openai/gpt-5.4->openai/gpt-5.5",
        "model-version:anthropic/claude-opus-4-5->anthropic/claude-opus-4-7",
    ]);
    write_applied_migrations(&config_path, &original);
    assert_eq!(read_applied_migrations(&config_path), original);
}
