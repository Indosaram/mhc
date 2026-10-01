use std::collections::BTreeMap;
use serde_json::Value;
use std::{path::{Path, PathBuf}, sync::Arc};
use crate::{watch_engine::{WatchTarget, WatchKind, WatchFilter}, extension_watch_scope::{is_loadable_extension_entry, is_scannable_extension_directory}};

pub struct ConfigReloadHandoffRegistry<T> { handoffs: BTreeMap<String, T> }
impl<T> Default for ConfigReloadHandoffRegistry<T> { fn default() -> Self { Self { handoffs: BTreeMap::new() } } }
impl<T> ConfigReloadHandoffRegistry<T> {
    pub fn set(&mut self, session_handle: String, handoff: T) { self.handoffs.insert(session_handle, handoff); }
    pub fn take(&mut self, session_handle: &str) -> Option<T> { self.handoffs.remove(session_handle) }
    pub fn delete(&mut self, session_handle: &str) { self.handoffs.remove(session_handle); }
}
pub struct ResolvedConfigReloadSettings { pub enabled: bool, pub debounce_ms: f64, pub watch: BTreeMap<String, bool> }
pub struct ActiveTarget { pub registration_id: String, pub target: WatchTarget, pub rearm_on_creation: Option<PathBuf> }
pub fn build_builtin_watch_targets(cwd: &Path, agent_dir: &Path, project_trusted: bool, settings: &ResolvedConfigReloadSettings, skill_paths: &[PathBuf]) -> Vec<ActiveTarget> {
    let mut targets = Vec::new();
    let json: Vec<_> = ["settings.jsonc", "settings.json", "models.json", "keybindings.json"].into_iter().filter(|name| {
        settings.watch[if name.starts_with("settings.") { "settings" } else if *name == "models.json" { "models" } else { "keybindings" }]
    }).map(PathBuf::from).collect();
    if !json.is_empty() { targets.push(active("builtin-global-json", agent_dir, WatchKind::Dir, Some(json), None, None)); }
    for (resource, id) in [("prompts", "builtin-global-prompts"), ("extensions", "builtin-global-extensions")] {
        if settings.watch[resource] { add_directory(&mut targets, id, &agent_dir.join(resource), resource == "extensions"); }
    }
    if settings.watch["skills"] {
        for (index, path) in skill_paths.iter().enumerate() {
            let path = if path.is_absolute() { path.clone() } else { cwd.join(path) };
            let id = format!("builtin-skill-{index}");
            if path.is_file() {
                if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) { targets.push(active(&id, parent, WatchKind::Dir, Some(vec![name.into()]), None, None)); }
            } else { add_directory(&mut targets, &id, &path, false); }
        }
    }
    if project_trusted {
        let project = crate::routine_settings::join_config_dir(cwd);
        if settings.watch.values().any(|enabled| *enabled) { targets.push(active("builtin-project-presence", cwd, WatchKind::Dir, Some(vec![maho_core::config::config_dir_name().into()]), None, (!project.is_dir()).then(|| project.clone()))); }
        if project.is_dir() {
            if settings.watch["settings"] { targets.push(active("builtin-project-settings", &project, WatchKind::Dir, Some(vec!["settings.jsonc".into(), "settings.json".into()]), None, None)); }
            for resource in ["prompts", "skills", "extensions"] { if settings.watch[resource] { add_directory(&mut targets, &format!("builtin-project-{resource}"), &project.join(resource), resource == "extensions"); } }
        }
    }
    targets
}
fn active(id: &str, path: &Path, kind: WatchKind, allow_list: Option<Vec<PathBuf>>, filter: Option<WatchFilter>, rearm_on_creation: Option<PathBuf>) -> ActiveTarget {
    ActiveTarget { registration_id: "builtin".into(), target: WatchTarget { id: id.into(), path: path.into(), kind, allow_list, filter }, rearm_on_creation }
}
fn add_directory(targets: &mut Vec<ActiveTarget>, id: &str, path: &Path, extensions: bool) {
    if path.is_dir() {
        let root = path.to_path_buf();
        let filter: Option<WatchFilter> = extensions.then(|| Arc::new(move |relative: &Path| is_loadable_extension_entry(&root, &relative.to_string_lossy()) || is_scannable_extension_directory(&root, &relative.to_string_lossy())) as WatchFilter);
        targets.push(active(id, path, WatchKind::DirRecursive, None, filter, None));
    } else {
        let mut parent = path.parent().unwrap_or(path).to_path_buf();
        while !parent.is_dir() { let Some(next) = parent.parent() else { break; }; parent = next.into(); }
        if let Some(segment) = path.strip_prefix(&parent).ok().and_then(|relative| relative.components().next()) {
            let segment = PathBuf::from(segment.as_os_str());
            targets.push(active(&format!("{id}-presence"), &parent, WatchKind::Dir, Some(vec![segment.clone()]), None, Some(parent.join(segment))));
        }
    }
}
pub fn resolve_config_reload_settings(global: &Value, project: &Value) -> ResolvedConfigReloadSettings {
    let global = global.get("configReload").filter(|value| value.is_object());
    let project = project.get("configReload").filter(|value| value.is_object());
    let enabled = project.and_then(|patch| patch.get("enabled")).and_then(Value::as_bool).or_else(|| global.and_then(|patch| patch.get("enabled")).and_then(Value::as_bool)).unwrap_or(true);
    let debounce = |patch: Option<&Value>| patch.and_then(|patch| patch.get("debounceMs")).and_then(Value::as_f64).filter(|value| value.is_finite() && *value >= 0.0).map(f64::floor);
    let watch = ["settings", "models", "keybindings", "prompts", "skills", "extensions"].into_iter().map(|key| {
        let field = |patch: Option<&Value>| patch.and_then(|patch| patch.get("watch")).and_then(|watch| watch.get(key)).and_then(Value::as_bool);
        (key.into(), field(project).or_else(|| field(global)).unwrap_or(true))
    }).collect();
    ResolvedConfigReloadSettings { enabled, debounce_ms: debounce(project).or_else(|| debounce(global)).unwrap_or(200.0), watch }
}
