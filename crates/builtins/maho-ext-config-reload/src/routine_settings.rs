use std::{collections::{BTreeMap, BTreeSet}, path::{Component, Path, PathBuf}};
use maho_core::{config::config_dir_name, settings_manager::parse_settings_json};
use crate::log::{ConfigReloadLogger, LogEvent, LogLevel};

const ROUTINE_SETTINGS_KEYS: &[&str] = &["defaultModel", "defaultProvider", "defaultThinkingLevel", "modelThinkingLevels", "modelLastOnThinkingLevels", "modelServiceTiers", "lastChangelogVersion", "changelogSeen", "tipsHistory"];

fn resolve(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut output = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => { output.pop(); }, Component::CurDir => {},
            Component::Normal(_) | Component::RootDir | Component::Prefix(_) => output.push(component.as_os_str()),
        }
    }
    output
}
pub fn join_config_dir(cwd: &Path) -> PathBuf { resolve(&cwd.join(config_dir_name())) }
pub fn is_settings_path(path: &Path, agent_dir: &Path, cwd: &Path) -> bool {
    let resolved = resolve(path);
    ["settings.jsonc", "settings.json"].iter().any(|name| resolved == resolve(&agent_dir.join(name)) || resolved == join_config_dir(cwd).join(name))
}
pub fn update_settings_content_snapshot(contents: &mut BTreeMap<PathBuf, String>, path: &Path) {
    let key = resolve(path);
    match std::fs::read_to_string(path) { Ok(content) => { contents.insert(key, content); }, Err(_) => { contents.remove(&key); } }
}
pub fn refresh_settings_content_snapshots(contents: &mut BTreeMap<PathBuf, String>, agent_dir: &Path, cwd: &Path) {
    contents.clear();
    for name in ["settings.jsonc", "settings.json"] {
        update_settings_content_snapshot(contents, &agent_dir.join(name));
        update_settings_content_snapshot(contents, &join_config_dir(cwd).join(name));
    }
}
pub fn is_routine_only_settings_change(previous: Option<&str>, next: Option<&str>) -> bool {
    let (Some(previous), Some(next)) = (previous, next) else { return false; };
    let (Ok(previous), Ok(next)) = (parse_settings_json(previous), parse_settings_json(next)) else { return false; };
    let keys: BTreeSet<_> = previous.keys().chain(next.keys()).collect();
    let changed: Vec<_> = keys.into_iter().filter(|key| !json_values_equal(previous.get(*key), next.get(*key))).collect();
    !changed.is_empty() && changed.iter().all(|key| ROUTINE_SETTINGS_KEYS.contains(&key.as_str()))
}
fn json_values_equal(previous: Option<&serde_json::Value>, next: Option<&serde_json::Value>) -> bool {
    use serde_json::Value;
    match (previous, next) {
        (Some(Value::Number(left)), Some(Value::Number(right))) => left.as_f64() == right.as_f64(),
        (Some(Value::Array(left)), Some(Value::Array(right))) => left.len() == right.len() && left.iter().zip(right).all(|(left, right)| json_values_equal(Some(left), Some(right))),
        (Some(Value::Object(left)), Some(Value::Object(right))) => {
            let ordered = |object: &serde_json::Map<String, Value>| {
                let mut keys: Vec<_> = object.keys().cloned().collect();
                keys.sort_by_key(|key| key.parse::<u32>().ok().filter(|index| *index != u32::MAX && index.to_string() == *key).map_or((1, 0), |index| (0, index)));
                keys
            };
            let left_keys = ordered(left);
            left.len() == right.len() && left_keys == ordered(right) && left_keys.iter().all(|key| json_values_equal(left.get(key), right.get(key)))
        },
        _ => previous == next,
    }
}
pub fn exclude_routine_only_settings_changes(paths: &[PathBuf], contents: &mut BTreeMap<PathBuf, String>, agent_dir: &Path, cwd: &Path, logger: &mut ConfigReloadLogger) -> Vec<PathBuf> {
    paths.iter().filter(|path| {
        if !is_settings_path(path, agent_dir, cwd) { return true; }
        let next = std::fs::read_to_string(path).ok();
        let routine = is_routine_only_settings_change(contents.get(&resolve(path)).map(String::as_str), next.as_deref());
        update_settings_content_snapshot(contents, path);
        if routine { logger.log(LogLevel::Debug, LogEvent::RoutineSettingsChangeSuppressed { path: &path.to_string_lossy() }); }
        !routine
    }).cloned().collect()
}
