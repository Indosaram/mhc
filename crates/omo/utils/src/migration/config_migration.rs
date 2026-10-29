use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde_json::{Map, Value, json};

use super::agent_names::{agent_name_for, migrate_agent_names};
use super::hook_names::migrate_hook_names;
use super::migrations_sidecar::{read_applied_migrations, write_applied_migrations};
use super::model_versions::migrate_model_versions;
use crate::atomic_write::write_file_atomically;
use crate::logger::log;

fn string_set(value: Option<&Value>) -> HashSet<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn migrate_section(
    copy: &mut Map<String, Value>,
    key: &str,
    existing: &HashSet<String>,
    label: &str,
) -> (bool, Vec<String>) {
    let Some(Value::Object(section)) = copy.get(key) else {
        return (false, Vec::new());
    };
    let result = migrate_model_versions(section, Some(existing));
    if result.changed {
        copy.insert(key.to_string(), Value::Object(result.migrated));
        log(&format!("Migrated model versions in {label} config"), None);
    }
    (result.changed, result.new_migrations)
}

fn render(config: &Map<String, Value>) -> String {
    let body = serde_json::to_string_pretty(config).unwrap_or_else(|_| "{}".to_string());
    format!("{body}\n")
}

/// Apply every legacy migration to `raw_config`, persist the result with a backup, and
/// record applied model migrations in the sidecar. Returns whether a write was needed;
/// `raw_config` is replaced in place with the migrated config either way.
pub fn migrate_config_file(
    config_path: impl AsRef<Path>,
    raw_config: &mut Map<String, Value>,
) -> bool {
    let config_path = config_path.as_ref();
    let mut copy = raw_config.clone();
    let mut needs_write = false;

    let in_config = string_set(copy.get("_migrations"));
    let inline_applied = string_set(copy.get("appliedMigrations"));
    let mut existing = read_applied_migrations(config_path);
    existing.extend(in_config.iter().cloned());
    existing.extend(inline_applied.iter().cloned());
    let had_legacy_state = !in_config.is_empty() || !inline_applied.is_empty();

    if let Some(Value::Object(agents)) = copy.get("agents") {
        let result = migrate_agent_names(agents);
        if result.changed {
            copy.insert("agents".to_string(), Value::Object(result.migrated));
            needs_write = true;
        }
    }
    let mut new_migrations = Vec::new();
    for (key, label) in [("agents", "agents"), ("categories", "categories")] {
        let (changed, applied) = migrate_section(&mut copy, key, &existing, label);
        needs_write |= changed;
        new_migrations.extend(applied);
    }

    let to_record: Vec<String> = new_migrations
        .into_iter()
        .filter(|key| !existing.contains(key))
        .collect();
    let mut full_set = existing.clone();
    full_set.extend(to_record.iter().cloned());
    let should_write_sidecar = !to_record.is_empty() || had_legacy_state;
    needs_write |= !to_record.is_empty();
    if had_legacy_state {
        copy.remove("appliedMigrations");
        needs_write = true;
    }
    if should_write_sidecar {
        let mut ordered: Vec<&String> = existing.iter().collect();
        ordered.sort();
        let mut entries: Vec<Value> = ordered.into_iter().map(|key| json!(key)).collect();
        entries.extend(to_record.iter().map(|key| json!(key)));
        copy.insert("_migrations".to_string(), Value::Array(entries));
        needs_write = true;
    }

    if copy.get("omo_agent").is_some_and(is_truthy)
        && let Some(omo_agent) = copy.remove("omo_agent")
    {
        copy.insert("sisyphus_agent".to_string(), omo_agent);
        needs_write = true;
    }

    if let Some(lsp) = copy.remove("lsp") {
        let dropped: Vec<String> = lsp
            .as_object()
            .map(|servers| servers.keys().cloned().collect())
            .unwrap_or_default();
        log(
            "Removed obsolete 'lsp' config key from oh-my-opencode config. Custom LSP servers are now configured in .opencode/lsp.json at the project root (consumed by the 'lsp' MCP server). Move any server definitions there to restore them.",
            Some(
                &json!({ "configPath": config_path.display().to_string(), "droppedServers": dropped }),
            ),
        );
        needs_write = true;
    }

    let moved_hashline = match copy.get_mut("experimental") {
        Some(Value::Object(experimental)) => experimental
            .remove("hashline_edit")
            .map(|hashline| (hashline, experimental.is_empty())),
        _ => None,
    };
    if let Some((hashline, emptied)) = moved_hashline {
        if emptied {
            copy.remove("experimental");
        }
        copy.entry("hashline_edit").or_insert(hashline);
        needs_write = true;
    }

    if let Some(Value::Array(disabled)) = copy.get("disabled_agents") {
        let migrated: Vec<Value> = disabled
            .iter()
            .map(|agent| {
                agent
                    .as_str()
                    .map_or_else(|| agent.clone(), |name| json!(agent_name_for(name)))
            })
            .collect();
        if migrated != *disabled {
            copy.insert("disabled_agents".to_string(), Value::Array(migrated));
            needs_write = true;
        }
    }

    if let Some(Value::Array(disabled)) = copy.get("disabled_hooks") {
        let hooks: Vec<String> = disabled
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        let result = migrate_hook_names(&hooks);
        if result.changed {
            copy.insert("disabled_hooks".to_string(), json!(result.migrated));
            needs_write = true;
        }
        if !result.removed.is_empty() {
            log(
                &format!(
                    "Removed obsolete hooks from disabled_hooks: {} (these hooks no longer exist in v3.0.0)",
                    result.removed.join(", ")
                ),
                None,
            );
        }
    }

    if needs_write {
        *raw_config = persist(config_path, copy, should_write_sidecar, &full_set);
    }
    needs_write
}

fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn persist(
    config_path: &Path,
    final_config: Map<String, Value>,
    should_write_sidecar: bool,
    full_set: &HashSet<String>,
) -> Map<String, Value> {
    let new_content = render(&final_config);
    let content_changed =
        fs::read_to_string(config_path).ok().as_deref() != Some(new_content.as_str());
    let timestamp = chrono::Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        .replace([':', '.'], "-");
    let backup_path = format!("{}.bak.{timestamp}", config_path.display());
    let backup_succeeded = content_changed && fs::copy(config_path, &backup_path).is_ok();

    let write_result = write_file_atomically(config_path, &new_content);
    if let Err(error) = &write_result {
        log(
            &format!(
                "Failed to write migrated config to {}:",
                config_path.display()
            ),
            Some(&json!(error.to_string())),
        );
    }
    let mut result = final_config;
    if write_result.is_ok()
        && should_write_sidecar
        && write_applied_migrations(config_path, full_set)
        && result.contains_key("_migrations")
    {
        let mut without_legacy = result.clone();
        without_legacy.remove("_migrations");
        match write_file_atomically(config_path, &render(&without_legacy)) {
            Ok(()) => result = without_legacy,
            Err(error) => log(
                &format!(
                    "Failed to remove legacy _migrations fallback from {}:",
                    config_path.display()
                ),
                Some(&json!(error.to_string())),
            ),
        }
    }
    let backup_message = if backup_succeeded {
        format!(" (backup: {backup_path})")
    } else {
        String::new()
    };
    if write_result.is_ok() {
        log(
            &format!(
                "Migrated config file: {}{backup_message}",
                config_path.display()
            ),
            None,
        );
    } else {
        log(
            &format!(
                "Applied migrated config in-memory for: {}{backup_message}",
                config_path.display()
            ),
            None,
        );
    }
    result
}
