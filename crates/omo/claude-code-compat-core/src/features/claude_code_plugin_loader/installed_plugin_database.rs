use super::{discovery_paths::get_installed_plugins_path, types::PluginInstallation};
use crate::shared::logger::log;
use serde_json::Value;
use std::{fs, path::Path};

pub fn load_installed_plugins(base_dir: Option<&Path>) -> Option<Value> {
    let path = get_installed_plugins_path(base_dir);
    if !path.exists() {
        return None;
    }
    match fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|content| serde_json::from_str(&content).map_err(|e| e.to_string()))
    {
        Ok(database) => Some(database),
        Err(error) => {
            log(
                &format!("Failed to load installed plugins database: {error}"),
                None,
            );
            None
        }
    }
}

pub fn extract_plugin_entries(db: &Value) -> Vec<(String, PluginInstallation)> {
    if let Some(entries) = db.as_array() {
        return entries
            .iter()
            .filter_map(|entry| {
                let name = entry.get("name")?.as_str()?;
                let marketplace = entry.get("marketplace")?.as_str()?;
                entry.get("installPath")?.as_str()?;
                let mut installation: PluginInstallation =
                    serde_json::from_value(entry.clone()).ok()?;
                installation
                    .installed_at
                    .clone_from(&installation.last_updated);
                Some((format!("{name}@{marketplace}"), installation))
            })
            .collect();
    }
    let Some(plugins) = db.get("plugins").and_then(Value::as_object) else {
        return Vec::new();
    };
    plugins
        .iter()
        .filter_map(|(key, value)| {
            let value = if db.get("version").and_then(Value::as_u64) == Some(1) {
                value
            } else {
                value.as_array()?.first()?
            };
            serde_json::from_value(value.clone())
                .ok()
                .map(|installation| (key.clone(), installation))
        })
        .collect()
}
